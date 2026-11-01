//! `bb export`: the listing of an S3 prefix as a CSV, Parquet or XLSX file.

use bucketbridge_core::Error;
use bucketbridge_core::export::{self, Request};
use serde::Serialize;

use crate::cli::ExportArgs;
use crate::context::Context;
use crate::output::{Failure, Output};

/// The record of `--json`: what was exported and where it went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExportRecord {
    /// The `s3://` URI that was listed.
    pub source: String,
    /// `csv`, `parquet` or `xlsx`.
    pub format: String,
    /// The file that was written (absolute).
    pub file: String,
    /// Whether every object below the prefix was listed.
    pub recursive: bool,
    /// Data rows, without the header.
    pub rows: u64,
    /// Size of the file in bytes.
    pub bytes: u64,
}

/// Lists the prefix (one folder level, or everything below it with
/// `--recursive`) and writes the rows to `--out`, replacing it when the
/// export is complete. Ctrl-C stops the export and removes its partial file.
pub async fn run(ctx: &Context, args: &ExportArgs, out: &mut Output) -> Result<(), Failure> {
    let dest = std::path::absolute(&args.out)
        .map_err(|err| Error::io(format!("resolving {}", args.out.display()), err))?;
    let format = export::Format::from(args.format);
    let request = Request {
        recursive: args.recursive,
        ..Request::new(args.uri.bucket(), args.uri.key(), format)
    };
    let outcome = export::export_listing(&ctx.s3, &request, &dest, Some(ctx.stop.token())).await?;
    let record = ExportRecord {
        source: args.uri.to_string(),
        format: format.to_string(),
        file: outcome.path,
        recursive: args.recursive,
        rows: outcome.rows,
        bytes: outcome.bytes,
    };
    out.change(&record, || line(&record))
}

/// `exported 12 rows from s3://bucket/prefix/ to /path/listing.csv (csv, 1234 bytes)`
fn line(record: &ExportRecord) -> String {
    format!(
        "exported {} from {} to {} ({}, {} bytes)",
        if record.rows == 1 {
            "1 row".to_owned()
        } else {
            format!("{} rows", record.rows)
        },
        record.source,
        record.file,
        record.format,
        record.bytes
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_names_the_source_the_file_and_the_size() {
        let mut record = ExportRecord {
            source: "s3://b/p/".into(),
            format: "csv".into(),
            file: "/tmp/out.csv".into(),
            recursive: false,
            rows: 12,
            bytes: 345,
        };
        assert_eq!(
            line(&record),
            "exported 12 rows from s3://b/p/ to /tmp/out.csv (csv, 345 bytes)"
        );
        record.rows = 1;
        assert!(
            line(&record).starts_with("exported 1 row from"),
            "{}",
            line(&record)
        );
    }

    #[test]
    fn field_names_are_stable() {
        let record = ExportRecord {
            source: "s3://b/".into(),
            format: "xlsx".into(),
            file: "/tmp/x.xlsx".into(),
            recursive: true,
            rows: 3,
            bytes: 9,
        };
        assert_eq!(
            serde_json::to_value(&record).unwrap(),
            serde_json::json!({
                "source": "s3://b/", "format": "xlsx", "file": "/tmp/x.xlsx",
                "recursive": true, "rows": 3, "bytes": 9
            })
        );
    }
}
