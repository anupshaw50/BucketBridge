//! The formats a preview decodes beyond plain text, JSON and images: XML
//! pretty-printing and table pages for CSV, TSV and Parquet.
//!
//! Everything here works on bytes or text that are already in memory and
//! never interprets them: the XML formatter is a text re-indenter that does
//! not expand entities, load a DTD or follow a `SYSTEM` identifier, and the
//! table decoders only produce cell text.

use bytes::Bytes;
use parquet::file::reader::{FileReader, SerializedFileReader};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Rows of a table in one preview page.
pub const TABLE_ROWS_PER_PAGE: u64 = 200;

/// Columns shown; further columns are left out.
const MAX_COLUMNS: usize = 200;

/// Characters of one cell shown; a longer cell is cut and ends in an ellipsis.
const MAX_CELL_CHARS: usize = 500;

/// Deepest element nesting the XML formatter indents.
const MAX_XML_DEPTH: usize = 256;

/// One page of a table (`PreviewTable` of `docs/desktop-api.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewTable {
    /// Column names: the first row of a CSV or TSV file, the schema of a
    /// Parquet file. A column the header does not name has an empty name.
    pub columns: Vec<String>,
    /// The rows of this page; a `None` cell is a null (Parquet).
    pub rows: Vec<Vec<Option<String>>>,
    /// Rows the table has in all (of the part that was read, when `truncated`).
    pub total_rows: u64,
    /// True when only the start of a CSV or TSV file that is larger than the
    /// preview limit was read.
    pub truncated: bool,
    /// True when columns beyond the first 200 were left out.
    pub columns_cut: bool,
}

/// The table formats a key name selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableFormat {
    /// Comma separated values.
    Csv,
    /// Tab separated values.
    Tsv,
    /// Apache Parquet.
    Parquet,
}

impl TableFormat {
    /// The format the object key's extension says, if any.
    pub fn from_key(key: &str) -> Option<TableFormat> {
        let name = key.to_ascii_lowercase();
        if name.ends_with(".csv") {
            Some(TableFormat::Csv)
        } else if name.ends_with(".tsv") || name.ends_with(".tab") {
            Some(TableFormat::Tsv)
        } else if name.ends_with(".parquet") || name.ends_with(".parq") {
            Some(TableFormat::Parquet)
        } else {
            None
        }
    }

    /// The media type reported for the format.
    pub fn media_type(self) -> &'static str {
        match self {
            TableFormat::Csv => "text/csv",
            TableFormat::Tsv => "text/tab-separated-values",
            TableFormat::Parquet => "application/vnd.apache.parquet",
        }
    }
}

/// How many pages a table of `rows` rows has (at least one).
pub fn table_page_count(rows: u64) -> u64 {
    rows.div_ceil(TABLE_ROWS_PER_PAGE).max(1)
}

fn clip(text: &str) -> String {
    match text.char_indices().nth(MAX_CELL_CHARS) {
        Some((at, _)) => format!("{}\u{2026}", &text[..at]),
        None => text.to_owned(),
    }
}

/// Page `page` of a CSV or TSV document held in `bytes`. `whole` says that
/// `bytes` is the whole file; otherwise the last, possibly cut, line is
/// dropped and the table is `truncated`. The first record is the header.
pub fn csv_table(bytes: &[u8], whole: bool, tab: bool, page: u64) -> PreviewTable {
    let mut data = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    if !whole {
        data = match data.iter().rposition(|byte| *byte == b'\n') {
            Some(end) => &data[..=end],
            None => &[],
        };
    }
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(if tab { b'\t' } else { b',' })
        .from_reader(data);
    let mut record = csv::ByteRecord::new();
    let mut columns: Vec<String> = Vec::new();
    let mut width = 0usize;
    let mut total = 0u64;
    let mut rows = Vec::new();
    let first = page * TABLE_ROWS_PER_PAGE;
    let mut header = true;
    let mut broken = false;
    loop {
        match reader.read_byte_record(&mut record) {
            Ok(true) => {}
            Ok(false) => break,
            Err(_) => {
                broken = true;
                break;
            }
        }
        width = width.max(record.len());
        let cells = |record: &csv::ByteRecord| -> Vec<String> {
            record
                .iter()
                .take(MAX_COLUMNS)
                .map(|cell| clip(&String::from_utf8_lossy(cell)))
                .collect()
        };
        if header {
            header = false;
            columns = cells(&record);
            continue;
        }
        if total >= first && (rows.len() as u64) < TABLE_ROWS_PER_PAGE {
            rows.push(cells(&record).into_iter().map(Some).collect());
        }
        total += 1;
    }
    let columns_cut = width > MAX_COLUMNS;
    columns.resize(width.min(MAX_COLUMNS).max(columns.len()), String::new());
    PreviewTable {
        columns,
        rows,
        total_rows: total,
        truncated: !whole || broken,
        columns_cut,
    }
}

/// Whether `bytes` start and end like a Parquet file.
pub fn is_parquet(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && bytes.starts_with(b"PAR1") && bytes.ends_with(b"PAR1")
}

/// Page `page` of a Parquet file held whole in `bytes`, decoded with the
/// crate's record reader (Snappy is the only compression codec built in).
/// Fails when the file cannot be read.
pub fn parquet_table(bytes: Vec<u8>, page: u64) -> Result<PreviewTable> {
    let fail = |err: parquet::errors::ParquetError| {
        Error::Usage(format!("this Parquet file cannot be previewed: {err}"))
    };
    let reader = SerializedFileReader::new(Bytes::from(bytes)).map_err(fail)?;
    let metadata = reader.metadata();
    let all: Vec<String> = metadata
        .file_metadata()
        .schema_descr()
        .columns()
        .iter()
        .map(|column| column.path().string())
        .collect();
    let columns_cut = all.len() > MAX_COLUMNS;
    let columns: Vec<String> = all.into_iter().take(MAX_COLUMNS).collect();
    let total: u64 = metadata
        .row_groups()
        .iter()
        .map(|group| u64::try_from(group.num_rows()).unwrap_or(0))
        .sum();
    let first = page * TABLE_ROWS_PER_PAGE;
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    let mut skipped = 0u64;
    for index in 0..metadata.num_row_groups() {
        let in_group = u64::try_from(metadata.row_group(index).num_rows()).unwrap_or(0);
        if skipped + in_group <= first {
            skipped += in_group;
            continue;
        }
        let group = reader.get_row_group(index).map_err(fail)?;
        let iter = group.get_row_iter(None).map_err(fail)?;
        let skip = usize::try_from(first.saturating_sub(skipped)).unwrap_or(0);
        for row in iter.skip(skip) {
            if rows.len() as u64 >= TABLE_ROWS_PER_PAGE {
                break;
            }
            let row = row.map_err(fail)?;
            rows.push(
                row.get_column_iter()
                    .take(MAX_COLUMNS)
                    .map(|(_, field)| match field {
                        parquet::record::Field::Null => None,
                        parquet::record::Field::Str(text) => Some(clip(text)),
                        other => Some(clip(&other.to_string())),
                    })
                    .collect(),
            );
        }
        skipped += in_group;
        if rows.len() as u64 >= TABLE_ROWS_PER_PAGE {
            break;
        }
    }
    Ok(PreviewTable {
        columns,
        rows,
        total_rows: total,
        truncated: false,
        columns_cut,
    })
}

/// The content type or the key says XML.
pub fn xml_hint(content_type: Option<&str>, key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    content_type.is_some_and(|ct| ct.to_ascii_lowercase().contains("xml"))
        || [
            ".xml", ".xsd", ".xsl", ".xslt", ".rss", ".atom", ".wsdl", ".kml", ".gpx", ".plist",
        ]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// The text starts like an XML document (a markup `<` first).
pub fn starts_like_xml(text: &str) -> bool {
    text.trim_start_matches('\u{feff}')
        .trim_start()
        .starts_with('<')
}

enum Token<'a> {
    /// `<?...?>`, `<!--...-->` or `<!DOCTYPE ...>`: shown as it is.
    Markup(&'a str),
    Start(&'a str, &'a str),
    End(&'a str),
    Empty(&'a str),
    Text(&'a str),
}

/// Index just past the `>` that closes the tag starting at `from`, skipping
/// quoted attribute values.
fn tag_end(text: &str, from: usize) -> Option<usize> {
    let mut quote: Option<u8> = None;
    for (at, byte) in text.bytes().enumerate().skip(from) {
        match (quote, byte) {
            (Some(q), b) if b == q => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(byte),
            (None, b'>') => return Some(at + 1),
            _ => {}
        }
    }
    None
}

/// End of a document type declaration, with its optional `[...]` subset.
fn doctype_end(text: &str, from: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    for (at, byte) in text.bytes().enumerate().skip(from) {
        match (quote, byte) {
            (Some(q), b) if b == q => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(byte),
            (None, b'[') => depth += 1,
            (None, b']') => depth = depth.saturating_sub(1),
            (None, b'>') if depth == 0 => return Some(at + 1),
            _ => {}
        }
    }
    None
}

fn is_name_start(ch: char) -> bool {
    ch.is_alphabetic() || ch == '_' || ch == ':'
}

fn tokenize(text: &str) -> Option<Vec<Token<'_>>> {
    let mut tokens = Vec::new();
    let mut at = 0usize;
    while at < text.len() {
        let rest = &text[at..];
        if !rest.starts_with('<') {
            let end = rest.find('<').map_or(text.len(), |n| at + n);
            tokens.push(Token::Text(&text[at..end]));
            at = end;
            continue;
        }
        let end = if rest.starts_with("<!--") {
            at + rest.find("-->")? + 3
        } else if rest.starts_with("<![CDATA[") {
            let end = at + rest.find("]]>")? + 3;
            tokens.push(Token::Text(&text[at..end]));
            at = end;
            continue;
        } else if rest.starts_with("<?") {
            at + rest.find("?>")? + 2
        } else if rest.starts_with("<!") {
            doctype_end(text, at)?
        } else {
            tag_end(text, at)?
        };
        let piece = &text[at..end];
        at = end;
        if piece.starts_with("<!") || piece.starts_with("<?") {
            tokens.push(Token::Markup(piece));
        } else if let Some(name) = piece.strip_prefix("</") {
            let name = name.strip_suffix('>')?.trim();
            if !name.chars().next().is_some_and(is_name_start) {
                return None;
            }
            tokens.push(Token::End(name));
        } else {
            let inner = piece.strip_prefix('<')?.strip_suffix('>')?;
            if !inner.chars().next().is_some_and(is_name_start) {
                return None;
            }
            let name_len = inner
                .find(|ch: char| ch.is_whitespace() || ch == '/')
                .unwrap_or(inner.len());
            let name = &inner[..name_len];
            if inner.ends_with('/') {
                tokens.push(Token::Empty(piece));
            } else {
                tokens.push(Token::Start(piece, name));
            }
        }
    }
    Some(tokens)
}

/// Re-indents an XML document with two spaces. Elements with only text stay
/// on one line, whitespace between elements is dropped, comments, processing
/// instructions, CDATA sections and the document type are kept as written.
///
/// This is text processing only: entity references are not expanded, a
/// document type is not loaded and no `SYSTEM` or `PUBLIC` identifier is
/// followed, so a hostile document cannot read a file, open a connection or
/// grow the output beyond a small multiple of its size. `None` when the text
/// is not well-formed in the sense that every tag closes in order.
pub fn pretty_xml(text: &str) -> Option<String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let tokens = tokenize(text)?;
    let mut out = String::with_capacity(text.len() + text.len() / 2);
    let mut stack: Vec<&str> = Vec::new();
    let mut roots = 0usize;
    let mut i = 0usize;
    let line = |out: &mut String, depth: usize, piece: &str| {
        if !out.is_empty() {
            out.push('\n');
        }
        for _ in 0..depth {
            out.push_str("  ");
        }
        out.push_str(piece);
    };
    while i < tokens.len() {
        match &tokens[i] {
            Token::Markup(piece) => line(&mut out, stack.len(), piece),
            Token::Text(raw) => {
                let trimmed = raw.trim();
                if !trimmed.is_empty() {
                    if stack.is_empty() {
                        return None;
                    }
                    line(&mut out, stack.len(), trimmed);
                }
            }
            Token::Empty(piece) => {
                if stack.is_empty() {
                    roots += 1;
                }
                line(&mut out, stack.len(), piece);
            }
            Token::Start(piece, name) => {
                if stack.is_empty() {
                    roots += 1;
                }
                // <a>text</a> and <a></a> stay on one line.
                let inline = match (tokens.get(i + 1), tokens.get(i + 2)) {
                    (Some(Token::Text(body)), Some(Token::End(end)))
                        if end == name && !body.contains('\n') =>
                    {
                        Some((body.trim(), 2))
                    }
                    (Some(Token::End(end)), _) if end == name => Some(("", 1)),
                    _ => None,
                };
                if let Some((body, skip)) = inline {
                    line(&mut out, stack.len(), &format!("{piece}{body}</{name}>"));
                    i += skip;
                } else {
                    line(&mut out, stack.len(), piece);
                    stack.push(name);
                    if stack.len() > MAX_XML_DEPTH {
                        return None;
                    }
                }
            }
            Token::End(name) => {
                if stack.pop() != Some(*name) {
                    return None;
                }
                line(&mut out, stack.len(), &format!("</{name}>"));
            }
        }
        i += 1;
    }
    (stack.is_empty() && roots == 1).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_is_indented_and_keeps_text_elements_on_one_line() {
        let xml = "<?xml version=\"1.0\"?>\n<!-- c -->\n<a x=\"1>2\"><b>hi</b>  <c/><d><e>\n</e></d>\n<f>x &amp; <![CDATA[<y>]]></f></a>";
        assert_eq!(
            pretty_xml(xml).unwrap(),
            "<?xml version=\"1.0\"?>\n<!-- c -->\n<a x=\"1>2\">\n  <b>hi</b>\n  <c/>\n  <d>\n    <e>\n    </e>\n  </d>\n  <f>\n    x &amp;\n    <![CDATA[<y>]]>\n  </f>\n</a>"
        );
    }

    #[test]
    fn xml_that_is_not_well_formed_is_left_alone() {
        for bad in [
            "<a><b></a>",
            "<a>",
            "</a>",
            "<a></a><b></b>",
            "text <a/>",
            "<a> 1 < 2 </a>",
            "<a",
            "<!-- open",
            "<a></a>tail",
        ] {
            assert_eq!(pretty_xml(bad), None, "{bad}");
        }
    }

    #[test]
    fn xml_entities_and_external_identifiers_are_never_resolved() {
        let xxe = "<?xml version=\"1.0\"?>\n<!DOCTYPE d [ <!ENTITY xxe SYSTEM \"file:///etc/passwd\"> <!ENTITY u SYSTEM \"http://127.0.0.1:9/x\"> ]>\n<d><v>&xxe;</v><w>&u;</w></d>";
        let pretty = pretty_xml(xxe).unwrap();
        assert!(
            pretty.contains("&xxe;") && pretty.contains("&u;"),
            "{pretty}"
        );
        assert!(pretty.contains("<!ENTITY xxe SYSTEM \"file:///etc/passwd\">"));
        assert!(!pretty.contains("root:"), "{pretty}");
        // A nested-entity bomb stays the size of its input.
        let bomb = "<!DOCTYPE l [ <!ENTITY a \"aaaaaaaaaa\"> <!ENTITY b \"&a;&a;&a;&a;&a;&a;&a;&a;\"> <!ENTITY c \"&b;&b;&b;&b;&b;&b;&b;&b;\"> ]>\n<l>&c;&c;&c;</l>";
        let out = pretty_xml(bomb).unwrap();
        assert!(out.len() < bomb.len() * 2, "{}", out.len());
        assert!(out.contains("<l>&c;&c;&c;</l>"));
    }

    #[test]
    fn xml_nesting_is_capped() {
        let deep = format!(
            "{}{}",
            "<a>".repeat(MAX_XML_DEPTH + 10),
            "</a>".repeat(MAX_XML_DEPTH + 10)
        );
        assert_eq!(pretty_xml(&deep), None);
    }

    #[test]
    fn xml_hints_and_start() {
        assert!(xml_hint(Some("application/xml"), "x"));
        assert!(xml_hint(None, "a/B.XML"));
        assert!(!xml_hint(Some("text/plain"), "a.txt"));
        assert!(starts_like_xml("\u{feff}  <a/>"));
        assert!(!starts_like_xml("hello"));
    }

    #[test]
    fn table_formats_follow_the_key() {
        assert_eq!(TableFormat::from_key("a/B.CSV"), Some(TableFormat::Csv));
        assert_eq!(TableFormat::from_key("a.tsv"), Some(TableFormat::Tsv));
        assert_eq!(
            TableFormat::from_key("a.parquet"),
            Some(TableFormat::Parquet)
        );
        assert_eq!(TableFormat::from_key("a.csv.gz"), None);
        assert_eq!(TableFormat::from_key("a.txt"), None);
    }

    fn numbered(rows: usize) -> String {
        let mut text = String::from("id,name\n");
        for n in 0..rows {
            text.push_str(&format!("{n},\"n,{n}\"\n"));
        }
        text
    }

    #[test]
    fn csv_pages_hold_200_rows_and_the_header_is_the_columns() {
        let text = numbered(450);
        let first = csv_table(text.as_bytes(), true, false, 0);
        assert_eq!(first.columns, ["id", "name"]);
        assert_eq!(first.total_rows, 450);
        assert_eq!(first.rows.len(), 200);
        assert_eq!(first.rows[1], [Some("1".into()), Some("n,1".into())]);
        assert!(!first.truncated);
        let last = csv_table(text.as_bytes(), true, false, 2);
        assert_eq!(last.rows.len(), 50);
        assert_eq!(last.rows[0][0].as_deref(), Some("400"));
        assert_eq!(table_page_count(450), 3);
        assert_eq!(table_page_count(0), 1);
        assert_eq!(table_page_count(200), 1);
        assert_eq!(table_page_count(201), 2);
    }

    #[test]
    fn csv_handles_quotes_newlines_ragged_rows_tabs_and_bom() {
        let text = "\u{feff}a,b\n\"x\ny\",\"q\"\"z\"\n1,2,3\n";
        let table = csv_table(text.as_bytes(), true, false, 0);
        assert_eq!(table.columns, ["a", "b", ""]);
        assert_eq!(table.rows[0][0].as_deref(), Some("x\ny"));
        assert_eq!(table.rows[0][1].as_deref(), Some("q\"z"));
        assert_eq!(table.rows[1].len(), 3);
        let tsv = csv_table(b"a\tb\n1\t2,3\n", true, true, 0);
        assert_eq!(tsv.rows[0][1].as_deref(), Some("2,3"));
        let header_only = csv_table(b"a,b\n", true, false, 0);
        assert!(header_only.rows.is_empty());
        assert_eq!(header_only.total_rows, 0);
    }

    #[test]
    fn a_cut_csv_drops_its_partial_line_and_is_truncated() {
        let table = csv_table(b"a,b\n1,2\n3,", false, false, 0);
        assert!(table.truncated);
        assert_eq!(table.total_rows, 1);
        assert!(csv_table(b"no newline", false, false, 0).columns.is_empty());
    }

    #[test]
    fn long_cells_and_wide_rows_are_cut() {
        let wide = vec!["x"; MAX_COLUMNS + 5].join(",");
        let text = format!("{wide}\n{}\n", "y".repeat(MAX_CELL_CHARS + 50));
        let table = csv_table(text.as_bytes(), true, false, 0);
        assert!(table.columns_cut);
        assert_eq!(table.columns.len(), MAX_COLUMNS);
        let cell = table.rows[0][0].as_deref().unwrap();
        assert_eq!(cell.chars().count(), MAX_CELL_CHARS + 1);
        assert!(cell.ends_with('\u{2026}'));
    }

    /// A Parquet file of `rows` rows (`id` and a `label` that is null for
    /// every third row), `group_rows` rows per row group.
    pub(crate) fn parquet_file(rows: i64, group_rows: i64) -> Vec<u8> {
        use parquet::basic::Compression;
        use parquet::data_type::{ByteArray, ByteArrayType, Int64Type};
        use parquet::file::properties::WriterProperties;
        use parquet::file::writer::SerializedFileWriter;
        use parquet::schema::parser::parse_message_type;
        use std::sync::Arc;
        let schema =
            parse_message_type("message m { required int64 id; optional binary label (STRING); }")
                .unwrap();
        let props = WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .build();
        let mut out = Vec::new();
        let mut writer =
            SerializedFileWriter::new(&mut out, Arc::new(schema), Arc::new(props)).unwrap();
        let mut start = 0;
        while start < rows {
            let end = (start + group_rows).min(rows);
            let mut group = writer.next_row_group().unwrap();
            let ids: Vec<i64> = (start..end).collect();
            let mut column = group.next_column().unwrap().unwrap();
            column
                .typed::<Int64Type>()
                .write_batch(&ids, None, None)
                .unwrap();
            column.close().unwrap();
            let labels: Vec<ByteArray> = (start..end)
                .filter(|n| n % 3 != 0)
                .map(|n| ByteArray::from(format!("row {n}").as_str()))
                .collect();
            let levels: Vec<i16> = (start..end).map(|n| i16::from(n % 3 != 0)).collect();
            let mut column = group.next_column().unwrap().unwrap();
            column
                .typed::<ByteArrayType>()
                .write_batch(&labels, Some(&levels), None)
                .unwrap();
            column.close().unwrap();
            group.close().unwrap();
            start = end;
        }
        writer.close().unwrap();
        out
    }

    #[test]
    fn parquet_pages_cross_row_groups_and_show_nulls() {
        let file = parquet_file(450, 120);
        assert!(is_parquet(&file));
        let first = parquet_table(file.clone(), 0).unwrap();
        assert_eq!(first.columns, ["id", "label"]);
        assert_eq!(first.total_rows, 450);
        assert_eq!(first.rows.len(), 200);
        assert_eq!(first.rows[0], [Some("0".into()), None]);
        assert_eq!(first.rows[1], [Some("1".into()), Some("row 1".into())]);
        let second = parquet_table(file.clone(), 1).unwrap();
        assert_eq!(second.rows[0][0].as_deref(), Some("200"));
        assert_eq!(second.rows[199][0].as_deref(), Some("399"));
        let third = parquet_table(file, 2).unwrap();
        assert_eq!(third.rows.len(), 50);
        assert_eq!(third.rows[49][0].as_deref(), Some("449"));
    }

    #[test]
    fn a_broken_parquet_file_is_an_error_not_a_panic() {
        assert!(!is_parquet(b"PAR1"));
        assert!(parquet_table(b"PAR1garbage-garbage-PAR1".to_vec(), 0).is_err());
    }
}
