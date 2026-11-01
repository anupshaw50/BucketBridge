//! Diagnostics and listing exports against real and scripted servers: a
//! `TestS3` (SigV4 is checked), a scripted `MockS3` (clock skew, access
//! denied, regions) and a `RawServer` that never answers (timeouts).
//! The network steps against TLS servers and proxies are unit tests of
//! `crates/core/src/diagnostics`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::time::{Duration, Instant};

use bucketbridge_core::diagnostics::{self, Network, Report, StepId, StepStatus, Timeouts};
use bucketbridge_core::export::{self, Format, Request};
use bucketbridge_core::model::FilterSpec;
use bucketbridge_core::{ClientOptions, Credentials, ErrorKind, S3};
use bucketbridge_tests::{MockResponse, MockS3, RawBehavior, RawServer, TestS3};
use futures::StreamExt;

fn timeouts(request: Duration) -> Timeouts {
    Timeouts {
        connect: Duration::from_secs(5),
        request,
    }
}

async fn probe(
    s3: &S3,
    account: &bucketbridge_core::model::Account,
    bucket: Option<&str>,
) -> Report {
    diagnostics::run(
        s3,
        account,
        bucket,
        &Network::default(),
        timeouts(Duration::from_secs(10)),
    )
    .await
    .unwrap()
}

fn step(report: &Report, id: StepId) -> (StepStatus, &str) {
    let step = report.step(id).unwrap();
    (step.status, step.code.as_str())
}

#[tokio::test]
async fn a_real_server_passes_every_check() {
    let server = TestS3::start().await;
    let bucket = server.new_bucket().await;
    let report = probe(server.ops(), server.account(), Some(&bucket)).await;
    assert!(report.ok, "{report:?}");
    assert_eq!(report.target, server.endpoint());
    assert_eq!(step(&report, StepId::Dns), (StepStatus::Skip, "ip_address"));
    assert_eq!(step(&report, StepId::Tcp), (StepStatus::Pass, "connected"));
    assert_eq!(step(&report, StepId::Proxy), (StepStatus::Skip, "none"));
    assert_eq!(step(&report, StepId::Tls), (StepStatus::Skip, "plain_http"));
    assert_eq!(
        step(&report, StepId::Credentials),
        (StepStatus::Pass, "valid")
    );
    // The server's date header gives a clock check.
    let detail = report
        .step(StepId::Credentials)
        .unwrap()
        .detail
        .clone()
        .unwrap();
    assert!(
        detail.starts_with("Clock check: This computer's clock"),
        "{detail}"
    );
    // This server reports no bucket region.
    assert_eq!(
        step(&report, StepId::Region),
        (StepStatus::Pass, "no_region_header")
    );

    // A bucket that does not exist is a result.
    let report = probe(server.ops(), server.account(), Some("no-such-bucket-0001")).await;
    assert!(!report.ok);
    assert_eq!(
        step(&report, StepId::Region),
        (StepStatus::Fail, "bucket_not_found")
    );
}

#[tokio::test]
async fn a_wrong_secret_and_an_unknown_key_are_told_apart() {
    let server = TestS3::start().await;
    let options = ClientOptions {
        max_attempts: 1,
        ..ClientOptions::default()
    };
    let wrong_secret = server.connect(
        &Credentials::new(server.access_key(), "not-the-right-secret-0000", None),
        &options,
    );
    let report = probe(&wrong_secret, server.account(), None).await;
    assert_eq!(
        step(&report, StepId::Credentials),
        (StepStatus::Fail, "bad_signature")
    );
    assert!(!report.ok);
    assert_eq!(
        step(&report, StepId::Region),
        (StepStatus::Skip, "no_bucket")
    );

    // The in-process server answers an unknown key with NotSignedUp: a
    // failure with the server's own words. S3 and MinIO say
    // InvalidAccessKeyId, which the scripted test below covers.
    let unknown = server.connect(
        &Credentials::new("AKIAUNKNOWN0000000000", "whatever", None),
        &options,
    );
    let report = probe(&unknown, server.account(), None).await;
    assert_eq!(
        step(&report, StepId::Credentials),
        (StepStatus::Fail, "failed")
    );
    assert!(
        report
            .step(StepId::Credentials)
            .unwrap()
            .summary
            .contains("not signed up")
    );
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains("not-the-right-secret-0000"), "{json}");
}

fn mock_response_with_date(error: MockResponse, minutes_behind: i64) -> MockResponse {
    let date = (chrono::Utc::now() - chrono::Duration::minutes(minutes_behind))
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string();
    error.with_header("date", &date)
}

#[tokio::test]
async fn scripted_answers_become_clear_findings() {
    let mock = MockS3::start(|request| {
        let bucket = request.path.trim_matches('/');
        match (
            request.method.as_str(),
            request.query_param("max-buckets").is_some(),
        ) {
            // ListBuckets of a server whose clock is 30 minutes behind.
            ("GET", true) => mock_response_with_date(
                MockResponse::error(
                    403,
                    "RequestTimeTooSkewed",
                    "The difference between the request time and the current time is too large.",
                ),
                30,
            ),
            ("HEAD", _) => match bucket {
                "secret" => {
                    MockResponse::empty(403).with_header("x-amz-bucket-region", "eu-west-1")
                }
                "moved" => {
                    MockResponse::empty(301).with_header("x-amz-bucket-region", "ap-south-1")
                }
                "there" => MockResponse::empty(200).with_header("x-amz-bucket-region", "eu-west-1"),
                _ => MockResponse::empty(404),
            },
            _ => MockResponse::error(400, "Unexpected", "unexpected request"),
        }
    });
    let account = mock.account(false);
    let s3 = mock.ops();

    let report = probe(&s3, &account, Some("secret")).await;
    let credentials = report.step(StepId::Credentials).unwrap();
    assert_eq!(
        (credentials.status, credentials.code.as_str()),
        (StepStatus::Fail, "clock_skew")
    );
    assert!(
        credentials
            .summary
            .contains("30 minutes ahead of the server's"),
        "{}",
        credentials.summary
    );
    // The server did answer, so the bucket probe still runs and still helps.
    let region = report.step(StepId::Region).unwrap();
    assert_eq!(
        (region.status, region.code.as_str()),
        (StepStatus::Fail, "access_denied")
    );
    assert!(
        region.summary.contains("It is in eu-west-1."),
        "{}",
        region.summary
    );

    let moved = probe(&s3, &account, Some("moved")).await;
    assert_eq!(step(&moved, StepId::Region), (StepStatus::Pass, "found"));
    assert!(
        moved
            .step(StepId::Region)
            .unwrap()
            .summary
            .contains("ap-south-1")
    );
    let there = probe(&s3, &account, Some("there")).await;
    let region = there.step(StepId::Region).unwrap();
    assert_eq!(
        (region.status, region.code.as_str()),
        (StepStatus::Pass, "found")
    );
    assert!(
        region
            .detail
            .as_deref()
            .unwrap()
            .contains("account region us-east-1")
    );
    assert_eq!(
        step(&probe(&s3, &account, Some("gone")).await, StepId::Region),
        (StepStatus::Fail, "bucket_not_found")
    );

    // Every probe is one signed request without retries.
    let list_requests = mock
        .requests()
        .iter()
        .filter(|r| r.query_param("max-buckets").is_some())
        .count();
    assert_eq!(list_requests, 4);

    // A key the server does not know.
    let unknown = MockS3::start(|_| {
        MockResponse::error(
            403,
            "InvalidAccessKeyId",
            "The AWS Access Key Id you provided does not exist in our records.",
        )
    });
    let report = probe(&unknown.ops(), &unknown.account(false), None).await;
    assert_eq!(
        step(&report, StepId::Credentials),
        (StepStatus::Fail, "invalid_access_key")
    );
    assert!(!report.ok);

    // A key that may not list buckets is still a valid key.
    let denied = MockS3::start(|request| {
        if request.method == "GET" {
            MockResponse::error(403, "AccessDenied", "Access Denied")
        } else {
            MockResponse::empty(200)
        }
    });
    let report = probe(&denied.ops(), &denied.account(false), None).await;
    assert!(report.ok, "{report:?}");
    assert_eq!(
        step(&report, StepId::Credentials),
        (StepStatus::Pass, "access_denied")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_that_never_answers_times_out_once() {
    let server = RawServer::start(RawBehavior::Silent);
    let s3 = server.ops_with(&ClientOptions::default());
    let started = Instant::now();
    let report = diagnostics::run(
        &s3,
        &server.account(),
        Some("some-bucket"),
        &Network::default(),
        timeouts(Duration::from_millis(800)),
    )
    .await
    .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(step(&report, StepId::Tcp), (StepStatus::Pass, "connected"));
    let credentials = report.step(StepId::Credentials).unwrap();
    assert_eq!(
        (credentials.status, credentials.code.as_str()),
        (StepStatus::Fail, "timeout")
    );
    assert!(credentials.elapsed_ms >= 700, "{}", credentials.elapsed_ms);
    // No answer at all: the region probe would only wait again.
    assert_eq!(step(&report, StepId::Region), (StepStatus::Skip, "blocked"));
    // One attempt, no retries.
    assert_eq!(server.requests(), 1);
    assert!(!report.ok);
}

// ---- export ----

async fn put_many(server: &TestS3, bucket: &str, keys: &[String]) {
    for chunk in keys.chunks(50) {
        futures::stream::iter(chunk)
            .for_each_concurrent(10, |key| server.put(bucket, key, "x"))
            .await;
    }
}

fn csv_rows(path: &Path) -> Vec<csv::StringRecord> {
    csv::Reader::from_path(path)
        .unwrap()
        .records()
        .map(Result::unwrap)
        .collect()
}

#[tokio::test]
async fn a_listing_of_many_pages_round_trips_through_every_reader() {
    use calamine::{Data, Reader, Xlsx, open_workbook};
    use parquet::file::reader::{FileReader, SerializedFileReader};
    use parquet::record::Field;

    let server = TestS3::start().await;
    let bucket = server.new_bucket().await;
    // More than one listing page, with keys that need care.
    let mut keys: Vec<String> = (0..1100).map(|i| format!("logs/{i:05}.log")).collect();
    keys.push("caf\u{e9}/\u{30d5}\u{30a1}\u{30a4}\u{30eb}.txt".to_owned());
    keys.push("=SUM(1+1)".to_owned());
    put_many(&server, &bucket, &keys).await;
    let mut expected = keys.clone();
    expected.sort();
    let dir = tempfile::tempdir().unwrap();

    let recursive = |format| Request {
        recursive: true,
        ..Request::new(&bucket, "", format)
    };

    let csv_path = dir.path().join("listing.csv");
    let outcome = export::export_listing(server.ops(), &recursive(Format::Csv), &csv_path, None)
        .await
        .unwrap();
    assert_eq!(outcome.rows, 1102);
    let rows = csv_rows(&csv_path);
    // The spreadsheet formula starter was defused; everything else is exact.
    let csv_keys: Vec<String> = rows.iter().map(|r| r[0].to_owned()).collect();
    let mut expected_csv: Vec<String> = expected
        .iter()
        .map(|k| {
            if k.starts_with('=') {
                format!("'{k}")
            } else {
                k.clone()
            }
        })
        .collect();
    expected_csv.sort();
    let mut sorted_csv = csv_keys.clone();
    sorted_csv.sort();
    assert_eq!(sorted_csv, expected_csv);
    assert!(rows.iter().all(|r| &r[1] == "1"));

    let xlsx_path = dir.path().join("listing.xlsx");
    let outcome = export::export_listing(server.ops(), &recursive(Format::Xlsx), &xlsx_path, None)
        .await
        .unwrap();
    assert_eq!(outcome.rows, 1102);
    let mut workbook: Xlsx<_> = open_workbook(&xlsx_path).unwrap();
    let range = workbook.worksheet_range("Objects").unwrap();
    assert_eq!(range.height(), 1103);
    let mut xlsx_keys: Vec<String> = range.rows().skip(1).map(|r| r[0].to_string()).collect();
    xlsx_keys.sort();
    assert_eq!(xlsx_keys, expected, "text cells are not defused");
    assert!(range.rows().skip(1).all(|r| r[1] == Data::Float(1.0)));

    let parquet_path = dir.path().join("listing.parquet");
    let outcome = export::export_listing(
        server.ops(),
        &recursive(Format::Parquet),
        &parquet_path,
        None,
    )
    .await
    .unwrap();
    assert_eq!(outcome.rows, 1102);
    let reader = SerializedFileReader::new(std::fs::File::open(&parquet_path).unwrap()).unwrap();
    assert_eq!(reader.metadata().file_metadata().num_rows(), 1102);
    let mut parquet_keys: Vec<String> = reader
        .get_row_iter(None)
        .unwrap()
        .map(
            |row| match row.unwrap().get_column_iter().next().unwrap().1 {
                Field::Str(key) => key.clone(),
                other => panic!("{other:?}"),
            },
        )
        .collect();
    parquet_keys.sort();
    assert_eq!(parquet_keys, expected);
}

#[tokio::test]
async fn one_level_and_filters_follow_the_object_list() {
    let server = TestS3::start().await;
    let bucket = server.new_bucket().await;
    put_many(
        &server,
        &bucket,
        &["a.txt", "b.log", "dir/c.txt", "dir/sub/d.txt"].map(str::to_owned),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("level.csv");

    // One level of a prefix: what the folder view shows, folders first.
    let request = Request::new(&bucket, "dir/", Format::Csv);
    export::export_listing(server.ops(), &request, &path, None)
        .await
        .unwrap();
    let keys: Vec<String> = csv_rows(&path).iter().map(|r| r[0].to_owned()).collect();
    assert_eq!(keys, ["dir/sub/", "dir/c.txt"]);

    // A filter works on the path below the prefix.
    let request = Request {
        recursive: true,
        filter: Some(FilterSpec {
            glob: Some("**/*.txt".into()),
            ..FilterSpec::default()
        }),
        ..Request::new(&bucket, "dir/", Format::Csv)
    };
    let outcome = export::export_listing(server.ops(), &request, &path, None)
        .await
        .unwrap();
    let keys: Vec<String> = csv_rows(&path).iter().map(|r| r[0].to_owned()).collect();
    assert_eq!(keys, ["dir/c.txt", "dir/sub/d.txt"]);
    assert_eq!(outcome.rows, 2);

    // Errors: a missing bucket, an invalid prefix.
    let missing = Request::new("no-such-bucket-0002", "", Format::Csv);
    let err = export::export_listing(server.ops(), &missing, &path, None)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound, "{err}");
    let long = Request::new(&bucket, "k".repeat(2000), Format::Csv);
    let err = export::export_listing(server.ops(), &long, &path, None)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Usage, "{err}");
    // The earlier successful export is still there, whole.
    assert_eq!(csv_rows(&path).len(), 2);
}

#[tokio::test]
async fn a_server_that_repeats_a_token_never_leaves_a_file() {
    let mock = MockS3::start(|request| {
        if request.query_param("list-type").is_some() {
            MockResponse::xml(
                "<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Name>b</Name><IsTruncated>true</IsTruncated><NextContinuationToken>again</NextContinuationToken><Contents><Key>k</Key><Size>1</Size></Contents></ListBucketResult>",
            )
        } else {
            MockResponse::error(400, "Unexpected", "unexpected request")
        }
    });
    let dir = tempfile::tempdir().unwrap();
    for recursive in [false, true] {
        let request = Request {
            recursive,
            ..Request::new("b", "", Format::Csv)
        };
        let err = export::export_listing(&mock.ops(), &request, &dir.path().join("x.csv"), None)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Remote, "{err}");
        assert!(err.to_string().contains("same continuation token"), "{err}");
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
