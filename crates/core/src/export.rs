//! Export of object listings (task T12): CSV, XLSX and Parquet.
//!
//! [`export_listing`] writes the listing of an S3 prefix to a file. The rows
//! have five columns, in every format: `key`, `size`, `last_modified`, `etag`
//! and `storage_class`. A folder row (a common prefix of a one-level listing)
//! has a key that ends in `/` and no other value. The folder marker of the
//! prefix itself is never a row, as in `bb ls`.
//!
//! # Memory
//!
//! The listing is streamed page by page. Rows travel to a writer thread in
//! batches of [`PAGE_ROWS`] over a channel that holds at most
//! [`CHANNEL_BATCHES`] batches, so listing and writing run at the same time
//! and a slow disk slows the listing down instead of filling memory.
//!
//! * CSV writes each row as it arrives.
//! * XLSX uses the constant-memory mode of `rust_xlsxwriter`: rows go to a
//!   temporary file as they are written, and the workbook is assembled at the
//!   end. A listing beyond Excel's 1,048,576 rows per sheet continues on
//!   further sheets (`Objects 2` and so on).
//! * Parquet is written with the crate's low-level writer (no Arrow). A row
//!   group is built from [`PARQUET_ROW_GROUP_ROWS`] rows, because the format
//!   stores one column after the other, so at most that many rows are in
//!   memory. Pages are compressed with Snappy.
//!
//! # Files
//!
//! The data goes to a temporary file in the destination folder, which is
//! renamed over the destination when everything was written. A failure, a
//! cancellation or a dropped future leaves no partial file and an existing
//! destination untouched.
//!
//! # Spreadsheets and untrusted keys
//!
//! Object keys come from whoever can write to a bucket. A CSV text field
//! that starts with `=`, `+`, `-`, `@`, a tab or a carriage return gets a
//! leading `'`, so that a spreadsheet program never runs a key as a formula
//! (the event-log export does the same). XLSX text is always written as a
//! string cell, never as a formula.

use std::collections::HashSet;
use std::fmt;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;

use chrono::{DateTime, SecondsFormat, Utc};
use futures::{Stream, StreamExt, TryStreamExt};
use parquet::basic::Compression;
use parquet::data_type::{ByteArray, ByteArrayType, Int64Type};
use parquet::errors::ParquetError;
use parquet::file::properties::WriterProperties;
use parquet::file::writer::{
    SerializedColumnWriter, SerializedFileWriter, SerializedRowGroupWriter,
};
use parquet::schema::parser::parse_message_type;
use rust_xlsxwriter::{Format as CellFormat, Workbook, XlsxError};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::{Error, Result};
use crate::filter::Filter;
use crate::model::{FilterSpec, ObjectEntry};
use crate::ops::{MAX_LIST_KEYS, S3};
use crate::s3uri::S3Uri;

/// The columns of every export, in order.
pub const COLUMNS: [&str; 5] = ["key", "size", "last_modified", "etag", "storage_class"];

/// Rows per batch from the listing to the writer.
pub const PAGE_ROWS: usize = 1000;

/// Batches the channel to the writer holds; the listing waits when the
/// writer is this far behind.
pub const CHANNEL_BATCHES: usize = 4;

/// Rows of one Parquet row group.
pub const PARQUET_ROW_GROUP_ROWS: usize = 50_000;

/// Rows of one Excel sheet, the header included.
pub const XLSX_MAX_SHEET_ROWS: u32 = 1_048_576;

/// Name of the first XLSX sheet.
const SHEET_NAME: &str = "Objects";

/// Days from 1899-12-30 (Excel's day 0) to 1970-01-01.
const EXCEL_UNIX_EPOCH_DAYS: f64 = 25_569.0;

/// Prefix of the temporary file next to the destination.
const TEMP_PREFIX: &str = ".bb-export-";

/// File format of an export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// Comma-separated values, UTF-8, with a header row.
    Csv,
    /// An Excel workbook.
    Xlsx,
    /// An Apache Parquet file.
    Parquet,
}

impl Format {
    /// Every format.
    pub const ALL: [Format; 3] = [Format::Csv, Format::Xlsx, Format::Parquet];

    /// The usual file name extension, which is also the name of the format.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Xlsx => "xlsx",
            Format::Parquet => "parquet",
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.extension())
    }
}

impl FromStr for Format {
    type Err = Error;

    /// Accepts `csv`, `xlsx` and `parquet`, ignoring case.
    fn from_str(text: &str) -> Result<Self> {
        Format::ALL
            .into_iter()
            .find(|format| format.extension().eq_ignore_ascii_case(text.trim()))
            .ok_or_else(|| {
                Error::Usage(format!(
                    "unknown export format '{text}'; use csv, parquet or xlsx"
                ))
            })
    }
}

/// What to export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// Bucket to list.
    pub bucket: String,
    /// Key prefix to list; empty for the whole bucket.
    pub prefix: String,
    /// Every object below the prefix, instead of one folder level.
    pub recursive: bool,
    /// File format.
    pub format: Format,
    /// Keep only the rows this filter accepts (paths relative to `prefix`).
    pub filter: Option<FilterSpec>,
}

impl Request {
    /// A one-level export of `prefix` without a filter.
    pub fn new(bucket: impl Into<String>, prefix: impl Into<String>, format: Format) -> Self {
        Request {
            bucket: bucket.into(),
            prefix: prefix.into(),
            recursive: false,
            format,
            filter: None,
        }
    }
}

/// What an export wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    /// The file that was written.
    pub path: String,
    /// Data rows, without the header.
    pub rows: u64,
    /// Size of the file in bytes.
    pub bytes: u64,
}

/// One row of an export.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    key: String,
    size: Option<u64>,
    last_modified: Option<DateTime<Utc>>,
    etag: Option<String>,
    storage_class: Option<String>,
}

impl From<ObjectEntry> for Row {
    fn from(entry: ObjectEntry) -> Self {
        Row {
            size: (!entry.is_prefix).then_some(entry.size),
            key: entry.key,
            last_modified: entry.last_modified,
            etag: entry.etag,
            storage_class: entry.storage_class,
        }
    }
}

type EntryStream = Pin<Box<dyn Stream<Item = Result<ObjectEntry>> + Send>>;

/// Writes the listing described by `request` to `dest`, replacing it.
///
/// `cancel` stops the export at the next opportunity: the result is
/// [`Error::Cancelled`] and nothing is left behind. Fails with `usage` for an
/// invalid bucket, prefix or filter, `io` when the file cannot be written
/// (a missing destination folder included) and with the listing's error
/// (`not_found`, `auth`, `remote`) otherwise; the destination is then
/// untouched.
pub async fn export_listing(
    s3: &S3,
    request: &Request,
    dest: &Path,
    cancel: Option<&CancellationToken>,
) -> Result<Outcome> {
    let uri = S3Uri::new(request.bucket.as_str(), request.prefix.as_str())?;
    let prefix = uri.key().to_owned();
    let entries: EntryStream = if request.recursive {
        Box::pin(s3.list_all_stream(&request.bucket, &prefix))
    } else {
        Box::pin(folder_entries(
            s3.clone(),
            request.bucket.clone(),
            prefix.clone(),
        ))
    };
    export_entries(entries, request, &prefix, dest, cancel).await
}

/// The entries of one folder level (what the object list shows), page by
/// page. A server that repeats a continuation token ends the stream with a
/// `remote` error.
fn folder_entries(
    s3: S3,
    bucket: String,
    prefix: String,
) -> impl Stream<Item = Result<ObjectEntry>> + Send + 'static {
    let start = Some((None::<String>, HashSet::<String>::new()));
    futures::stream::try_unfold(start, move |state| {
        let s3 = s3.clone();
        let bucket = bucket.clone();
        let prefix = prefix.clone();
        async move {
            let Some((token, mut seen)) = state else {
                return Ok(None);
            };
            let page = s3
                .list_objects(&bucket, &prefix, false, token.as_deref(), MAX_LIST_KEYS)
                .await?;
            let next = match page.next_token {
                None => None,
                Some(next) if !seen.insert(next.clone()) => {
                    return Err(Error::Remote(format!(
                        "listing s3://{bucket}/{prefix}: the server returned the same continuation token twice; stopping to avoid an endless listing"
                    )));
                }
                Some(next) => Some((Some(next), seen)),
            };
            Ok(Some((page.entries, next)))
        }
    })
    .map_ok(|entries| futures::stream::iter(entries.into_iter().map(Ok)))
    .try_flatten()
}

/// Message from the listing side to the writer thread.
enum Message {
    /// Rows to write.
    Rows(Vec<Row>),
    /// The listing is complete; without it the writer discards its file.
    Done,
}

/// Why the listing side stopped before the end.
enum Stop {
    Listing(Error),
    Writer,
    Cancelled,
}

async fn export_entries(
    mut entries: EntryStream,
    request: &Request,
    prefix: &str,
    dest: &Path,
    cancel: Option<&CancellationToken>,
) -> Result<Outcome> {
    let filter = match &request.filter {
        Some(spec) => Some(Filter::compile(spec)?).filter(|filter| !filter.is_unrestricted()),
        None => None,
    };
    let folder = dest
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_owned();
    let temp = tokio::task::spawn_blocking(move || {
        tempfile::Builder::new()
            .prefix(TEMP_PREFIX)
            .tempfile_in(&folder)
            .map_err(|err| {
                Error::io(
                    format!(
                        "cannot create a file for the export in {}",
                        folder.display()
                    ),
                    err,
                )
            })
    })
    .await
    .map_err(background_failure)??;

    let (sender, receiver) = mpsc::channel(CHANNEL_BATCHES);
    let writer = {
        let (format, dest) = (request.format, dest.to_owned());
        tokio::task::spawn_blocking(move || write_file(format, temp, receiver, &dest))
    };
    let fed = feed(&mut entries, filter.as_ref(), prefix, &sender, cancel).await;
    if fed.is_ok() {
        // A writer that is gone reports why when it is joined below.
        let _gone = sender.send(Message::Done).await;
    }
    drop(sender);
    let written = writer.await.map_err(background_failure)?;
    match (fed, written) {
        (Ok(()), Ok(Some(outcome))) => Ok(outcome),
        (Ok(()), Ok(None)) => Err(Error::Internal(
            "the export writer stopped before the listing ended".into(),
        )),
        (Err(Stop::Listing(error)), _) => Err(error),
        (Err(Stop::Cancelled), _) => Err(Error::Cancelled),
        (Ok(()) | Err(Stop::Writer), Err(error)) => Err(error),
        (Err(Stop::Writer), Ok(_)) => Err(Error::Internal(
            "the export writer stopped before the listing ended".into(),
        )),
    }
}

fn background_failure(error: tokio::task::JoinError) -> Error {
    Error::Internal(format!("a background task failed: {error}"))
}

/// Pulls entries, keeps the wanted ones and hands them to the writer in
/// batches.
async fn feed(
    entries: &mut EntryStream,
    filter: Option<&Filter>,
    prefix: &str,
    sender: &mpsc::Sender<Message>,
    cancel: Option<&CancellationToken>,
) -> std::result::Result<(), Stop> {
    let mut batch: Vec<Row> = Vec::with_capacity(PAGE_ROWS);
    loop {
        let next = match cancel {
            Some(token) => tokio::select! {
                biased;
                () = token.cancelled() => return Err(Stop::Cancelled),
                next = entries.next() => next,
            },
            None => entries.next().await,
        };
        let Some(entry) = next else { break };
        let entry = entry.map_err(Stop::Listing)?;
        if wanted(&entry, prefix, filter) {
            batch.push(Row::from(entry));
        }
        if batch.len() >= PAGE_ROWS {
            send(sender, std::mem::take(&mut batch), cancel).await?;
        }
    }
    if !batch.is_empty() {
        send(sender, batch, cancel).await?;
    }
    Ok(())
}

async fn send(
    sender: &mpsc::Sender<Message>,
    rows: Vec<Row>,
    cancel: Option<&CancellationToken>,
) -> std::result::Result<(), Stop> {
    let sending = sender.send(Message::Rows(rows));
    let sent = match cancel {
        Some(token) => tokio::select! {
            biased;
            () = token.cancelled() => return Err(Stop::Cancelled),
            sent = sending => sent,
        },
        None => sending.await,
    };
    sent.map_err(|_| Stop::Writer)
}

/// Whether an entry is a row: not the folder marker of the prefix, and
/// accepted by the filter.
fn wanted(entry: &ObjectEntry, prefix: &str, filter: Option<&Filter>) -> bool {
    if !prefix.is_empty() && entry.key == prefix {
        return false;
    }
    filter.is_none_or(|filter| filter.matches_object(entry, prefix))
}

/// The writer thread: receives the batches, writes the file and moves it to
/// `dest`. `Ok(None)` when the listing side ended without `Done` (an error,
/// a cancellation); the temporary file is removed when `temp` drops.
fn write_file(
    format: Format,
    mut temp: tempfile::NamedTempFile,
    mut receiver: mpsc::Receiver<Message>,
    dest: &Path,
) -> Result<Option<Outcome>> {
    let failed = |err: io::Error| Error::io(format!("cannot write {}", dest.display()), err);
    let mut rows = 0u64;
    {
        let mut sink = Sink::create(format, temp.as_file_mut())?;
        loop {
            match receiver.blocking_recv() {
                Some(Message::Rows(batch)) => {
                    rows += batch.len() as u64;
                    sink.write(batch)?;
                }
                Some(Message::Done) => break,
                None => return Ok(None),
            }
        }
        sink.finish()?;
    }
    let file = temp.as_file();
    file.sync_all().map_err(failed)?;
    let bytes = file.metadata().map_err(failed)?.len();
    temp.persist(dest).map_err(|err| failed(err.error))?;
    Ok(Some(Outcome {
        path: dest.to_string_lossy().into_owned(),
        rows,
        bytes,
    }))
}

/// The writer of one format.
enum Sink<'a> {
    Csv(CsvSink<'a>),
    // Boxed: a workbook is much larger than the other writers.
    Xlsx(Box<XlsxSink<'a>>),
    Parquet(ParquetSink<'a>),
}

impl<'a> Sink<'a> {
    fn create(format: Format, file: &'a mut File) -> Result<Self> {
        Ok(match format {
            Format::Csv => Sink::Csv(CsvSink::new(file)?),
            Format::Xlsx => Sink::Xlsx(Box::new(XlsxSink::new(file, XLSX_MAX_SHEET_ROWS)?)),
            Format::Parquet => Sink::Parquet(ParquetSink::new(file, PARQUET_ROW_GROUP_ROWS)?),
        })
    }

    fn write(&mut self, rows: Vec<Row>) -> Result<()> {
        match self {
            Sink::Csv(sink) => sink.write(&rows),
            Sink::Xlsx(sink) => sink.write(&rows),
            Sink::Parquet(sink) => sink.write(rows),
        }
    }

    fn finish(self) -> Result<()> {
        match self {
            Sink::Csv(sink) => sink.finish(),
            Sink::Xlsx(sink) => sink.finish(),
            Sink::Parquet(sink) => sink.finish(),
        }
    }
}

// ---- CSV ----

struct CsvSink<'a> {
    writer: csv::Writer<BufWriter<&'a mut File>>,
}

impl<'a> CsvSink<'a> {
    fn new(file: &'a mut File) -> Result<Self> {
        let mut writer = csv::Writer::from_writer(BufWriter::new(file));
        writer.write_record(COLUMNS).map_err(csv_error)?;
        Ok(CsvSink { writer })
    }

    fn write(&mut self, rows: &[Row]) -> Result<()> {
        for row in rows {
            let size = row.size.map(|size| size.to_string()).unwrap_or_default();
            let time = row
                .last_modified
                .map(|time| time.to_rfc3339_opts(SecondsFormat::AutoSi, true))
                .unwrap_or_default();
            let key = defuse(&row.key);
            let etag = defuse(row.etag.as_deref().unwrap_or_default());
            let class = defuse(row.storage_class.as_deref().unwrap_or_default());
            self.writer
                .write_record([&*key, size.as_str(), time.as_str(), &*etag, &*class])
                .map_err(csv_error)?;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<()> {
        self.writer
            .flush()
            .map_err(|err| Error::io("cannot write CSV", err))
    }
}

fn csv_error(error: csv::Error) -> Error {
    match error.into_kind() {
        csv::ErrorKind::Io(err) => Error::io("cannot write CSV", err),
        other => Error::Internal(format!("cannot write CSV: {other:?}")),
    }
}

/// Prefixes text that a spreadsheet would run as a formula with `'`.
fn defuse(text: &str) -> std::borrow::Cow<'_, str> {
    if text.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        std::borrow::Cow::Owned(format!("'{text}"))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

// ---- XLSX ----

struct XlsxSink<'a> {
    workbook: Workbook,
    out: &'a mut File,
    header: CellFormat,
    size: CellFormat,
    time: CellFormat,
    /// Sheets added so far; the last one is being written.
    sheets: usize,
    /// The next row of the current sheet.
    row: u32,
    max_rows: u32,
}

impl<'a> XlsxSink<'a> {
    fn new(out: &'a mut File, max_rows: u32) -> Result<Self> {
        let mut sink = XlsxSink {
            workbook: Workbook::new(),
            out,
            header: CellFormat::new().set_bold(),
            size: CellFormat::new().set_num_format("#,##0"),
            time: CellFormat::new().set_num_format("yyyy-mm-dd hh:mm:ss"),
            sheets: 0,
            row: 0,
            max_rows,
        };
        sink.add_sheet()?;
        Ok(sink)
    }

    fn add_sheet(&mut self) -> Result<()> {
        let name = match self.sheets {
            0 => SHEET_NAME.to_owned(),
            count => format!("{SHEET_NAME} {}", count + 1),
        };
        let sheet = self
            .workbook
            .add_worksheet_with_constant_memory()
            .map_err(xlsx_error)?;
        sheet.set_name(&name).map_err(xlsx_error)?;
        for (column, width) in [(0, 60.0), (1, 14.0), (2, 20.0), (3, 36.0), (4, 18.0)] {
            sheet.set_column_width(column, width).map_err(xlsx_error)?;
        }
        sheet.set_freeze_panes(1, 0).map_err(xlsx_error)?;
        for (column, title) in (0u16..).zip(COLUMNS) {
            sheet
                .write_string_with_format(0, column, title, &self.header)
                .map_err(xlsx_error)?;
        }
        self.sheets += 1;
        self.row = 1;
        Ok(())
    }

    /// Adds the filter buttons over the rows of the current sheet.
    fn close_sheet(&mut self) -> Result<()> {
        let last = self.row.saturating_sub(1);
        self.workbook
            .worksheet_from_index(self.sheets - 1)
            .map_err(xlsx_error)?
            .autofilter(0, 0, last, 4)
            .map_err(xlsx_error)?;
        Ok(())
    }

    fn write(&mut self, rows: &[Row]) -> Result<()> {
        for row in rows {
            if self.row >= self.max_rows {
                self.close_sheet()?;
                self.add_sheet()?;
            }
            let at = self.row;
            let sheet = self
                .workbook
                .worksheet_from_index(self.sheets - 1)
                .map_err(xlsx_error)?;
            // Strings are written as strings, whatever they start with.
            sheet.write_string(at, 0, &row.key).map_err(xlsx_error)?;
            if let Some(size) = row.size {
                // An object is at most 5 TiB, far below the 2^53 an f64 holds.
                sheet
                    .write_number_with_format(at, 1, size as f64, &self.size)
                    .map_err(xlsx_error)?;
            }
            if let Some(time) = row.last_modified {
                let days = time.timestamp_millis() as f64 / 86_400_000.0 + EXCEL_UNIX_EPOCH_DAYS;
                sheet
                    .write_number_with_format(at, 2, days, &self.time)
                    .map_err(xlsx_error)?;
            }
            if let Some(etag) = &row.etag {
                sheet.write_string(at, 3, etag).map_err(xlsx_error)?;
            }
            if let Some(class) = &row.storage_class {
                sheet.write_string(at, 4, class).map_err(xlsx_error)?;
            }
            self.row += 1;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<()> {
        self.close_sheet()?;
        let mut out = BufWriter::new(self.out);
        self.workbook.save_to_writer(&mut out).map_err(xlsx_error)?;
        out.flush()
            .map_err(|err| Error::io("cannot write the XLSX file", err))
    }
}

fn xlsx_error(error: XlsxError) -> Error {
    match error {
        XlsxError::IoError(err) => Error::io("cannot write the XLSX file", err),
        other => Error::Internal(format!("cannot write the XLSX file: {other}")),
    }
}

// ---- Parquet ----

/// `isAdjustedToUTC` is true: the times are instants.
const PARQUET_SCHEMA: &str = "
message listing {
  required binary key (STRING);
  optional int64 size;
  optional int64 last_modified (TIMESTAMP(MILLIS,true));
  optional binary etag (STRING);
  optional binary storage_class (STRING);
}";

struct ParquetSink<'a> {
    writer: SerializedFileWriter<BufWriter<&'a mut File>>,
    /// Rows of the row group being collected.
    pending: Vec<Row>,
    group_rows: usize,
}

impl<'a> ParquetSink<'a> {
    fn new(file: &'a mut File, group_rows: usize) -> Result<Self> {
        let schema = parse_message_type(PARQUET_SCHEMA).map_err(parquet_error)?;
        let properties = WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .set_created_by(format!("bucketbridge {}", env!("CARGO_PKG_VERSION")))
            .build();
        let writer =
            SerializedFileWriter::new(BufWriter::new(file), Arc::new(schema), Arc::new(properties))
                .map_err(parquet_error)?;
        Ok(ParquetSink {
            writer,
            pending: Vec::new(),
            group_rows,
        })
    }

    fn write(&mut self, rows: Vec<Row>) -> Result<()> {
        self.pending.extend(rows);
        while self.pending.len() >= self.group_rows {
            let rest = self.pending.split_off(self.group_rows);
            let group = std::mem::replace(&mut self.pending, rest);
            self.write_group(group)?;
        }
        Ok(())
    }

    /// Writes `rows` as one row group, column by column.
    fn write_group(&mut self, rows: Vec<Row>) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut group = self.writer.next_row_group().map_err(parquet_error)?;

        let keys: Vec<ByteArray> = rows.iter().map(|row| row.key.as_str().into()).collect();
        let mut column = next_column(&mut group)?;
        column
            .typed::<ByteArrayType>()
            .write_batch(&keys, None, None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;

        let (sizes, levels) = optional_values(&rows, |row| {
            row.size.map(|size| i64::try_from(size).unwrap_or(i64::MAX))
        });
        let mut column = next_column(&mut group)?;
        column
            .typed::<Int64Type>()
            .write_batch(&sizes, Some(&levels), None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;

        let (times, levels) = optional_values(&rows, |row| {
            row.last_modified.map(|time| time.timestamp_millis())
        });
        let mut column = next_column(&mut group)?;
        column
            .typed::<Int64Type>()
            .write_batch(&times, Some(&levels), None)
            .map_err(parquet_error)?;
        column.close().map_err(parquet_error)?;

        for (values, levels) in [
            optional_values(&rows, |row| row.etag.as_deref().map(ByteArray::from)),
            optional_values(&rows, |row| {
                row.storage_class.as_deref().map(ByteArray::from)
            }),
        ] {
            let mut column = next_column(&mut group)?;
            column
                .typed::<ByteArrayType>()
                .write_batch(&values, Some(&levels), None)
                .map_err(parquet_error)?;
            column.close().map_err(parquet_error)?;
        }
        group.close().map_err(parquet_error)?;
        Ok(())
    }

    fn finish(mut self) -> Result<()> {
        let rest = std::mem::take(&mut self.pending);
        self.write_group(rest)?;
        // Writes the footer and flushes the buffered writer.
        self.writer.finish().map_err(parquet_error)?;
        Ok(())
    }
}

fn next_column<'g, W: Write + Send>(
    group: &'g mut SerializedRowGroupWriter<'_, W>,
) -> Result<SerializedColumnWriter<'g>> {
    group
        .next_column()
        .map_err(parquet_error)?
        .ok_or_else(|| Error::Internal("the Parquet schema has fewer columns than expected".into()))
}

/// The present values of an optional column and its definition levels (1
/// present, 0 null), as Parquet wants them.
fn optional_values<T>(rows: &[Row], value: impl Fn(&Row) -> Option<T>) -> (Vec<T>, Vec<i16>) {
    let mut values = Vec::with_capacity(rows.len());
    let mut levels = Vec::with_capacity(rows.len());
    for row in rows {
        match value(row) {
            Some(value) => {
                values.push(value);
                levels.push(1);
            }
            None => levels.push(0),
        }
    }
    (values, levels)
}

fn parquet_error(error: ParquetError) -> Error {
    match error {
        ParquetError::External(source) => match source.downcast::<io::Error>() {
            Ok(io) => Error::io("cannot write the Parquet file", *io),
            Err(other) => Error::Internal(format!("cannot write the Parquet file: {other}")),
        },
        other => Error::Internal(format!("cannot write the Parquet file: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use chrono::TimeZone;

    fn time(secs: i64, millis: u32) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, millis * 1_000_000).unwrap()
    }

    fn object(key: &str, size: u64) -> Row {
        Row {
            key: key.to_owned(),
            size: Some(size),
            last_modified: Some(time(1_793_000_000, 0)),
            etag: Some("9b2cf535f27731c974343645a3985328".to_owned()),
            storage_class: Some("STANDARD".to_owned()),
        }
    }

    /// Rows that stress every format: separators, quotes, line breaks,
    /// non-ASCII text, formula starters, a control character, missing values.
    fn sample_rows() -> Vec<Row> {
        vec![
            object("plain/a.txt", 5),
            Row {
                last_modified: Some(time(1_793_000_123, 456)),
                ..object("commas, \"quotes\" and\nnew lines.txt", 1_234_567_890)
            },
            object("caf\u{e9}/\u{30d5}\u{30a1}\u{30a4}\u{30eb}.txt", 0),
            object("=HYPERLINK(\"http://example.invalid\",\"x\")", 7),
            object("-leading-dash/+plus", 8),
            Row {
                etag: Some("@etag".to_owned()),
                ..object("@at", 9)
            },
            object("control\u{1}char", 10),
            Row {
                key: "photos/".to_owned(),
                size: None,
                last_modified: None,
                etag: None,
                storage_class: None,
            },
            Row {
                key: "empty-values".to_owned(),
                size: Some(0),
                last_modified: None,
                etag: None,
                storage_class: None,
            },
        ]
    }

    fn write_rows(format: Format, rows: &[Row]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        {
            let mut sink = Sink::create(format, file.as_file_mut()).unwrap();
            // Two batches, as the writer thread receives them.
            let (first, second) = rows.split_at(rows.len() / 2);
            sink.write(first.to_vec()).unwrap();
            sink.write(second.to_vec()).unwrap();
            sink.finish().unwrap();
        }
        file
    }

    #[test]
    fn formats_parse_print_and_serialize() {
        for (text, format) in [
            ("csv", Format::Csv),
            ("XLSX", Format::Xlsx),
            (" Parquet ", Format::Parquet),
        ] {
            assert_eq!(text.parse::<Format>().unwrap(), format);
        }
        let err = "json".parse::<Format>().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
        assert!(err.to_string().contains("csv, parquet or xlsx"), "{err}");
        for format in Format::ALL {
            assert_eq!(format.to_string(), format.extension());
            assert_eq!(
                serde_json::to_string(&format).unwrap(),
                format!("\"{}\"", format.extension())
            );
            assert_eq!(
                serde_json::from_str::<Format>(&format!("\"{format}\"")).unwrap(),
                format
            );
        }
    }

    #[test]
    fn csv_round_trips_and_defuses_formulas() {
        let file = write_rows(Format::Csv, &sample_rows());
        let mut reader = csv::Reader::from_path(file.path()).unwrap();
        assert_eq!(reader.headers().unwrap(), COLUMNS.to_vec());
        let records: Vec<csv::StringRecord> = reader.records().map(Result::unwrap).collect();
        let expected: Vec<[&str; 5]> = vec![
            [
                "plain/a.txt",
                "5",
                "2026-10-26T07:33:20Z",
                "9b2cf535f27731c974343645a3985328",
                "STANDARD",
            ],
            [
                "commas, \"quotes\" and\nnew lines.txt",
                "1234567890",
                "2026-10-26T07:35:23.456Z",
                "9b2cf535f27731c974343645a3985328",
                "STANDARD",
            ],
            [
                "caf\u{e9}/\u{30d5}\u{30a1}\u{30a4}\u{30eb}.txt",
                "0",
                "2026-10-26T07:33:20Z",
                "9b2cf535f27731c974343645a3985328",
                "STANDARD",
            ],
            [
                "'=HYPERLINK(\"http://example.invalid\",\"x\")",
                "7",
                "2026-10-26T07:33:20Z",
                "9b2cf535f27731c974343645a3985328",
                "STANDARD",
            ],
            [
                "'-leading-dash/+plus",
                "8",
                "2026-10-26T07:33:20Z",
                "9b2cf535f27731c974343645a3985328",
                "STANDARD",
            ],
            ["'@at", "9", "2026-10-26T07:33:20Z", "'@etag", "STANDARD"],
            [
                "control\u{1}char",
                "10",
                "2026-10-26T07:33:20Z",
                "9b2cf535f27731c974343645a3985328",
                "STANDARD",
            ],
            ["photos/", "", "", "", ""],
            ["empty-values", "0", "", "", ""],
        ];
        assert_eq!(records.len(), expected.len());
        for (record, expected) in records.iter().zip(&expected) {
            assert_eq!(record.iter().collect::<Vec<_>>(), expected.to_vec());
        }
    }

    #[test]
    fn xlsx_round_trips_with_typed_cells() {
        use calamine::{Data, Reader, Xlsx, open_workbook};
        let rows = sample_rows();
        let file = write_rows(Format::Xlsx, &rows);
        let mut workbook: Xlsx<_> = open_workbook(file.path()).unwrap();
        assert_eq!(workbook.sheet_names(), [SHEET_NAME]);
        let range = workbook.worksheet_range(SHEET_NAME).unwrap();
        let cells: Vec<&[Data]> = range.rows().collect();
        assert_eq!(cells.len(), rows.len() + 1);
        let header: Vec<String> = cells[0].iter().map(ToString::to_string).collect();
        assert_eq!(header, COLUMNS);
        for (cells, row) in cells[1..].iter().zip(&rows) {
            // Text cells hold the key as it is: nothing is defused, nothing
            // becomes a formula.
            assert_eq!(cells[0], Data::String(row.key.clone()), "{}", row.key);
            match row.size {
                Some(size) => assert_eq!(cells[1], Data::Float(size as f64)),
                None => assert_eq!(cells[1], Data::Empty),
            }
            match row.last_modified {
                Some(time) => {
                    let Data::DateTime(cell) = &cells[2] else {
                        panic!("{} has no date: {:?}", row.key, cells[2])
                    };
                    let expected = time.timestamp_millis() as f64 / 86_400_000.0 + 25_569.0;
                    assert!((cell.as_f64() - expected).abs() < 1e-9, "{}", row.key);
                }
                None => assert_eq!(cells[2], Data::Empty),
            }
            assert_eq!(cells[3], row.etag.clone().map_or(Data::Empty, Data::String));
            assert_eq!(
                cells[4],
                row.storage_class.clone().map_or(Data::Empty, Data::String)
            );
        }
    }

    #[test]
    fn xlsx_continues_on_another_sheet_when_one_is_full() {
        use calamine::{Reader, Xlsx, open_workbook};
        let rows: Vec<Row> = (0..7).map(|i| object(&format!("k{i}"), i)).collect();
        let mut file = tempfile::NamedTempFile::new().unwrap();
        {
            // Header plus 3 rows per sheet.
            let mut sink = XlsxSink::new(file.as_file_mut(), 4).unwrap();
            sink.write(&rows).unwrap();
            sink.finish().unwrap();
        }
        let mut workbook: Xlsx<_> = open_workbook(file.path()).unwrap();
        assert_eq!(
            workbook.sheet_names(),
            ["Objects", "Objects 2", "Objects 3"]
        );
        let mut keys = Vec::new();
        for name in ["Objects", "Objects 2", "Objects 3"] {
            let range = workbook.worksheet_range(name).unwrap();
            let sheet: Vec<String> = range.rows().map(|row| row[0].to_string()).collect();
            assert_eq!(sheet[0], "key", "{name} repeats the header");
            keys.extend(sheet.into_iter().skip(1));
        }
        assert_eq!(keys, ["k0", "k1", "k2", "k3", "k4", "k5", "k6"]);
    }

    #[test]
    fn xlsx_header_row_is_frozen_and_filtered() {
        use std::io::Read;
        let file = write_rows(Format::Xlsx, &sample_rows());
        let mut archive = zip::ZipArchive::new(file.reopen().unwrap()).unwrap();
        let mut sheet = String::new();
        archive
            .by_name("xl/worksheets/sheet1.xml")
            .unwrap()
            .read_to_string(&mut sheet)
            .unwrap();
        assert!(sheet.contains("<pane"), "{sheet}");
        assert!(sheet.contains("state=\"frozen\""), "{sheet}");
        assert!(sheet.contains("<autoFilter ref=\"A1:E10\""), "{sheet}");
        // Text is always an inline string, never a formula.
        assert!(!sheet.contains("<f>"), "{sheet}");
    }

    #[test]
    fn parquet_round_trips_with_types_and_nulls() {
        use parquet::file::reader::{FileReader, SerializedFileReader};
        use parquet::record::Field;
        let rows = sample_rows();
        let mut file = tempfile::NamedTempFile::new().unwrap();
        {
            // Four rows per row group: three groups for nine rows.
            let mut sink = ParquetSink::new(file.as_file_mut(), 4).unwrap();
            sink.write(rows.clone()).unwrap();
            sink.finish().unwrap();
        }
        let reader = SerializedFileReader::new(file.reopen().unwrap()).unwrap();
        let metadata = reader.metadata();
        assert_eq!(metadata.num_row_groups(), 3);
        assert_eq!(
            metadata.file_metadata().created_by(),
            Some(format!("bucketbridge {}", env!("CARGO_PKG_VERSION")).as_str())
        );
        assert_eq!(
            metadata.row_group(0).column(0).compression(),
            Compression::SNAPPY
        );
        let schema = metadata.file_metadata().schema_descr();
        let names: Vec<&str> = schema.columns().iter().map(|c| c.name()).collect();
        assert_eq!(names, COLUMNS);

        let read: Vec<_> = reader
            .get_row_iter(None)
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert_eq!(read.len(), rows.len());
        for (read, row) in read.iter().zip(&rows) {
            let fields: Vec<&Field> = read.get_column_iter().map(|(_, field)| field).collect();
            assert_eq!(fields[0], &Field::Str(row.key.clone()));
            assert_eq!(
                fields[1],
                &row.size
                    .map_or(Field::Null, |size| Field::Long(size as i64))
            );
            assert_eq!(
                fields[2],
                &row.last_modified
                    .map_or(Field::Null, |time| Field::TimestampMillis(
                        time.timestamp_millis()
                    ))
            );
            assert_eq!(fields[3], &row.etag.clone().map_or(Field::Null, Field::Str));
            assert_eq!(
                fields[4],
                &row.storage_class.clone().map_or(Field::Null, Field::Str)
            );
        }
    }

    #[test]
    fn an_empty_listing_is_a_valid_file_in_every_format() {
        for format in Format::ALL {
            let file = write_rows(format, &[]);
            let bytes = std::fs::read(file.path()).unwrap();
            assert!(!bytes.is_empty(), "{format}");
            match format {
                Format::Csv => assert_eq!(
                    String::from_utf8(bytes).unwrap().trim_end(),
                    COLUMNS.join(",")
                ),
                Format::Xlsx => assert_eq!(&bytes[..2], b"PK"),
                Format::Parquet => {
                    assert_eq!(&bytes[..4], b"PAR1");
                    assert_eq!(&bytes[bytes.len() - 4..], b"PAR1");
                }
            }
        }
    }

    // ---- the export pipeline over synthetic entry streams ----

    fn entry(key: &str, size: u64) -> ObjectEntry {
        ObjectEntry {
            key: key.to_owned(),
            size,
            etag: Some(format!("etag-{size}")),
            last_modified: Some(time(1_793_000_000, 0)),
            storage_class: Some("STANDARD".to_owned()),
            ..ObjectEntry::default()
        }
    }

    fn stream_of(entries: Vec<Result<ObjectEntry>>) -> EntryStream {
        Box::pin(futures::stream::iter(entries))
    }

    fn csv_request() -> Request {
        Request::new("bucket", "", Format::Csv)
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn every_entry_is_written_across_batches() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.csv");
        let count = PAGE_ROWS * 2 + 345;
        let entries = (0..count)
            .map(|i| Ok(entry(&format!("k{i:06}"), i as u64)))
            .collect();
        let outcome = export_entries(stream_of(entries), &csv_request(), "", &dest, None)
            .await
            .unwrap();
        assert_eq!(outcome.rows, count as u64);
        assert_eq!(outcome.path, dest.to_string_lossy());
        assert_eq!(outcome.bytes, std::fs::metadata(&dest).unwrap().len());
        let mut reader = csv::Reader::from_path(&dest).unwrap();
        let keys: Vec<String> = reader.records().map(|r| r.unwrap()[0].to_owned()).collect();
        assert_eq!(keys.len(), count);
        assert_eq!(keys[0], "k000000");
        assert_eq!(keys[count - 1], format!("k{:06}", count - 1));
        assert_eq!(
            files_in(dir.path()),
            ["out.csv"],
            "no temporary file is left"
        );
    }

    #[tokio::test]
    async fn the_destination_is_replaced_only_by_a_complete_export() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.csv");
        std::fs::write(&dest, "old contents").unwrap();

        // A listing that fails half way leaves the old file alone.
        let failing = vec![
            Ok(entry("a", 1)),
            Ok(entry("b", 2)),
            Err(Error::Remote("the server went away".into())),
            Ok(entry("c", 3)),
        ];
        let err = export_entries(stream_of(failing), &csv_request(), "", &dest, None)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Remote);
        assert!(err.to_string().contains("went away"), "{err}");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "old contents");
        assert_eq!(files_in(dir.path()), ["out.csv"]);

        // A complete export replaces it.
        export_entries(
            stream_of(vec![Ok(entry("a", 1))]),
            &csv_request(),
            "",
            &dest,
            None,
        )
        .await
        .unwrap();
        let text = std::fs::read_to_string(&dest).unwrap();
        assert!(text.starts_with("key,size,last_modified,etag,storage_class\n"));
        assert!(
            text.contains("a,1,2026-10-26T07:33:20Z,etag-1,STANDARD"),
            "{text}"
        );
        assert_eq!(files_in(dir.path()), ["out.csv"]);
    }

    #[tokio::test]
    async fn a_failure_after_many_batches_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.xlsx");
        let mut entries: Vec<Result<ObjectEntry>> = (0..PAGE_ROWS * 3)
            .map(|i| Ok(entry(&format!("k{i}"), 1)))
            .collect();
        entries.push(Err(Error::Auth("access denied".into())));
        let request = Request::new("bucket", "", Format::Xlsx);
        let err = export_entries(stream_of(entries), &request, "", &dest, None)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Auth);
        assert!(
            files_in(dir.path()).is_empty(),
            "{:?}",
            files_in(dir.path())
        );
    }

    #[tokio::test]
    async fn cancelling_stops_the_export_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.parquet");
        let token = CancellationToken::new();
        // Some entries, then a listing that never answers.
        let entries: EntryStream = Box::pin(
            futures::stream::iter((0..10).map(|i| Ok(entry(&format!("k{i}"), 1))))
                .chain(futures::stream::pending()),
        );
        let request = Request::new("bucket", "", Format::Parquet);
        let cancelling = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            cancelling.cancel();
        });
        let err = export_entries(entries, &request, "", &dest, Some(&token))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Cancelled);
        assert!(
            files_in(dir.path()).is_empty(),
            "{:?}",
            files_in(dir.path())
        );
    }

    #[tokio::test]
    async fn a_dropped_export_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.csv");
        let entries: EntryStream = Box::pin(
            futures::stream::iter((0..5).map(|i| Ok(entry(&format!("k{i}"), 1))))
                .chain(futures::stream::pending()),
        );
        let request = csv_request();
        let export = export_entries(entries, &request, "", &dest, None);
        // Give it time to start writing, then drop it, as `select!` does.
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), export).await;
        // The writer thread notices the closed channel and removes its file.
        for _ in 0..100 {
            if files_in(dir.path()).is_empty() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("left behind: {:?}", files_in(dir.path()));
    }

    #[tokio::test]
    async fn a_missing_folder_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("missing").join("out.csv");
        let err = export_entries(
            stream_of(vec![Ok(entry("a", 1))]),
            &csv_request(),
            "",
            &dest,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Io, "{err}");
        assert!(err.to_string().contains("missing"), "{err}");
    }

    #[tokio::test]
    async fn the_destination_cannot_be_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let err = export_entries(
            stream_of(vec![Ok(entry("a", 1))]),
            &csv_request(),
            "",
            dir.path(),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Io, "{err}");
        assert!(dir.path().exists());
    }

    #[tokio::test]
    async fn the_prefix_marker_is_skipped_and_folders_have_no_size() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.csv");
        let entries = vec![
            Ok(entry("photos/", 0)),
            Ok(ObjectEntry::prefix("photos/2024/")),
            Ok(entry("photos/a.jpg", 3)),
        ];
        let outcome = export_entries(stream_of(entries), &csv_request(), "photos/", &dest, None)
            .await
            .unwrap();
        assert_eq!(outcome.rows, 2);
        let mut reader = csv::Reader::from_path(&dest).unwrap();
        let records: Vec<csv::StringRecord> = reader.records().map(Result::unwrap).collect();
        assert_eq!(
            records[0].iter().collect::<Vec<_>>(),
            ["photos/2024/", "", "", "", ""]
        );
        assert_eq!(&records[1][0], "photos/a.jpg");
        assert_eq!(&records[1][1], "3");
    }

    #[tokio::test]
    async fn the_filter_keeps_only_matching_rows() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.csv");
        let entries = vec![
            Ok(entry("logs/a.gz", 10)),
            Ok(entry("logs/b.txt", 10)),
            Ok(entry("logs/big.gz", 5000)),
            Ok(ObjectEntry::prefix("logs/old/")),
            Ok(entry("other/c.gz", 10)),
        ];
        let request = Request {
            filter: Some(FilterSpec {
                glob: Some("*.gz".into()),
                max_size: Some(100),
                ..FilterSpec::default()
            }),
            ..csv_request()
        };
        let outcome = export_entries(stream_of(entries), &request, "logs/", &dest, None)
            .await
            .unwrap();
        // Entries outside the prefix never match; a folder stays only when
        // the filter has no prefix criterion that excludes it.
        let mut reader = csv::Reader::from_path(&dest).unwrap();
        let keys: Vec<String> = reader.records().map(|r| r.unwrap()[0].to_owned()).collect();
        assert_eq!(keys, ["logs/a.gz", "logs/old/"]);
        assert_eq!(outcome.rows, 2);

        // An invalid filter is a usage error before anything is written.
        let request = Request {
            filter: Some(FilterSpec {
                glob: Some("a[b".into()),
                ..FilterSpec::default()
            }),
            ..csv_request()
        };
        let err = export_entries(stream_of(Vec::new()), &request, "", &dest, None)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
    }

    #[tokio::test]
    async fn every_format_exports_through_the_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        for format in Format::ALL {
            let dest = dir.path().join(format!("out.{format}"));
            let entries = (0..2500).map(|i| Ok(entry(&format!("k{i}"), i))).collect();
            let request = Request::new("bucket", "", format);
            let outcome = export_entries(stream_of(entries), &request, "", &dest, None)
                .await
                .unwrap();
            assert_eq!(outcome.rows, 2500, "{format}");
            assert!(outcome.bytes > 0);
        }
        assert_eq!(files_in(dir.path()).len(), 3);
    }
}
