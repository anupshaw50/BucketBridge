//! Previews of CSV, TSV and Parquet objects as pages of rows, and of XML as
//! pretty-printed text, against the in-process server.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use bucketbridge_core::ErrorKind;
use bucketbridge_core::preview::{PreviewKind, PreviewOptions};
use bucketbridge_tests::TestS3;
use parquet::basic::Compression;
use parquet::data_type::Int64Type;
use parquet::file::properties::WriterProperties;
use parquet::file::writer::SerializedFileWriter;
use parquet::schema::parser::parse_message_type;

/// A Parquet file with an `id` column of `rows` rows, 70 rows per row group.
fn parquet_ids(rows: i64) -> Vec<u8> {
    let schema = parse_message_type("message m { required int64 id; }").unwrap();
    let props = WriterProperties::builder()
        .set_compression(Compression::SNAPPY)
        .build();
    let mut out = Vec::new();
    let mut writer =
        SerializedFileWriter::new(&mut out, Arc::new(schema), Arc::new(props)).unwrap();
    let mut start = 0;
    while start < rows {
        let end = (start + 70).min(rows);
        let ids: Vec<i64> = (start..end).collect();
        let mut group = writer.next_row_group().unwrap();
        let mut column = group.next_column().unwrap().unwrap();
        column
            .typed::<Int64Type>()
            .write_batch(&ids, None, None)
            .unwrap();
        column.close().unwrap();
        group.close().unwrap();
        start = end;
    }
    writer.close().unwrap();
    out
}

#[tokio::test]
async fn csv_and_parquet_objects_come_back_as_pages_of_rows() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let mut csv = String::from("id,name\n");
    for n in 0..450 {
        csv.push_str(&format!("{n},\"name {n}\"\n"));
    }
    s3.put(&bucket, "data/people.csv", csv).await;
    s3.put(&bucket, "data/ids.parquet", parquet_ids(450)).await;
    let options = PreviewOptions::default();
    for key in ["data/people.csv", "data/ids.parquet"] {
        let first = s3
            .ops()
            .preview_object(&bucket, key, None, 0, &options)
            .await
            .unwrap();
        assert_eq!(first.kind, PreviewKind::Table, "{key}");
        assert_eq!((first.page, first.page_count, first.page_size), (0, 3, 200));
        let table = first.table.unwrap();
        assert_eq!(table.total_rows, 450);
        assert_eq!(table.rows.len(), 200);
        assert_eq!(table.columns[0], "id");
        let last = s3
            .ops()
            .preview_object(&bucket, key, None, 2, &options)
            .await
            .unwrap();
        assert_eq!((last.offset, last.length), (400, 50));
        assert_eq!(
            last.table.unwrap().rows[49][0].as_deref(),
            Some("449"),
            "{key}"
        );
        let past = s3
            .ops()
            .preview_object(&bucket, key, None, 3, &options)
            .await
            .unwrap_err();
        assert_eq!(past.kind(), ErrorKind::Usage, "{key}");
    }
}

#[tokio::test]
async fn a_table_key_with_other_content_is_shown_as_it_is() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    s3.put(&bucket, "a.parquet", vec![0u8, 1, 2, 3, 4, 5]).await;
    s3.put(&bucket, "b.csv", vec![0u8, 1, 2, 3, 4, 5]).await;
    s3.put(&bucket, "empty.csv", Vec::<u8>::new()).await;
    let options = PreviewOptions::default();
    for (key, kind) in [
        ("a.parquet", PreviewKind::Binary),
        ("b.csv", PreviewKind::Binary),
        ("empty.csv", PreviewKind::Empty),
    ] {
        let answer = s3
            .ops()
            .preview_object(&bucket, key, None, 0, &options)
            .await
            .unwrap();
        assert_eq!(answer.kind, kind, "{key}");
    }
}

#[tokio::test]
async fn a_csv_above_the_budget_shows_its_start_and_a_large_parquet_is_too_large() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    let mut csv = String::from("n\n");
    for n in 0..2000 {
        csv.push_str(&format!("{n}\n"));
    }
    s3.put(&bucket, "big.csv", csv).await;
    s3.put(&bucket, "big.parquet", parquet_ids(2000)).await;
    let options = PreviewOptions {
        max_image_bytes: 1000,
        ..PreviewOptions::default()
    };
    let csv = s3
        .ops()
        .preview_object(&bucket, "big.csv", None, 0, &options)
        .await
        .unwrap();
    let table = csv.table.unwrap();
    assert!(table.truncated);
    assert!(table.total_rows > 100 && table.total_rows < 2000);
    let parquet = s3
        .ops()
        .preview_object(&bucket, "big.parquet", None, 0, &options)
        .await
        .unwrap();
    assert_eq!(parquet.kind, PreviewKind::TooLarge);
    assert!(parquet.table.is_none());
}

#[tokio::test]
async fn xml_is_pretty_printed_without_resolving_entities() {
    let s3 = TestS3::start().await;
    let bucket = s3.new_bucket().await;
    s3.put(&bucket, "a.xml", "<a><b>1</b><c/></a>".to_owned())
        .await;
    let xxe = "<!DOCTYPE d [ <!ENTITY x SYSTEM \"file:///etc/passwd\"> ]><d>&x;</d>";
    s3.put(&bucket, "evil.xml", xxe.to_owned()).await;
    s3.put(&bucket, "broken.xml", "<a><b></a>".to_owned()).await;
    let options = PreviewOptions::default();
    let ok = s3
        .ops()
        .preview_object(&bucket, "a.xml", None, 0, &options)
        .await
        .unwrap();
    assert_eq!(ok.kind, PreviewKind::Text);
    assert!(ok.formatted);
    assert_eq!(ok.media_type.as_deref(), Some("application/xml"));
    assert_eq!(ok.text.as_deref(), Some("<a>\n  <b>1</b>\n  <c/>\n</a>"));
    let evil = s3
        .ops()
        .preview_object(&bucket, "evil.xml", None, 0, &options)
        .await
        .unwrap();
    let text = evil.text.unwrap();
    assert!(
        text.contains("<d>&x;</d>") && !text.contains("root:"),
        "{text}"
    );
    let broken = s3
        .ops()
        .preview_object(&bucket, "broken.xml", None, 0, &options)
        .await
        .unwrap();
    assert!(!broken.formatted);
    assert_eq!(broken.text.as_deref(), Some("<a><b></a>"));
}
