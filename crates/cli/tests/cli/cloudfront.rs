//! `bb cloudfront`: run in-process with CloudFront replaced by scripted
//! answers (`aws-smithy-mocks`) injected through the secrets hook.

use std::sync::{Arc, Mutex};

use bucketbridge_cli::i18n::trf;

use aws_smithy_mocks::{MockResponseInterceptor, Rule, RuleMode, create_mock_http_client, mock};
use aws_smithy_runtime_api::client::orchestrator::HttpResponse;
use aws_smithy_runtime_api::http::StatusCode;
use aws_smithy_types::body::SdkBody;
use bucketbridge_core::aws_sdk_cloudfront::Client;
use bucketbridge_core::aws_sdk_cloudfront::operation::create_distribution::CreateDistributionOutput;
use bucketbridge_core::aws_sdk_cloudfront::operation::create_invalidation::CreateInvalidationOutput;
use bucketbridge_core::aws_sdk_cloudfront::operation::create_origin_access_control::CreateOriginAccessControlOutput;
use bucketbridge_core::aws_sdk_cloudfront::operation::get_distribution::GetDistributionOutput;
use bucketbridge_core::aws_sdk_cloudfront::operation::get_distribution_config::GetDistributionConfigOutput;
use bucketbridge_core::aws_sdk_cloudfront::operation::get_invalidation::GetInvalidationOutput;
use bucketbridge_core::aws_sdk_cloudfront::operation::list_distributions::ListDistributionsOutput;
use bucketbridge_core::aws_sdk_cloudfront::operation::list_invalidations::ListInvalidationsOutput;
use bucketbridge_core::aws_sdk_cloudfront::operation::update_distribution::UpdateDistributionOutput;
use bucketbridge_core::aws_sdk_cloudfront::primitives::DateTime;
use bucketbridge_core::aws_sdk_cloudfront::types::{
    Aliases, CacheBehaviors, CustomErrorResponses, DefaultCacheBehavior, Distribution,
    DistributionConfig, DistributionList, DistributionSummary, GeoRestriction, GeoRestrictionType,
    HttpVersion, Invalidation, InvalidationBatch, InvalidationList, InvalidationSummary, Origin,
    OriginAccessControl, Origins, Paths, PriceClass, Restrictions, ViewerCertificate,
    ViewerProtocolPolicy,
};
use serde_json::json;

use super::*;

const ETAG: &str = "E-CURRENT";

fn origin(id: &str, domain: &str) -> Origin {
    Origin::builder()
        .id(id)
        .domain_name(domain)
        .build()
        .unwrap()
}

fn behavior(target: &str) -> DefaultCacheBehavior {
    DefaultCacheBehavior::builder()
        .target_origin_id(target)
        .viewer_protocol_policy(ViewerProtocolPolicy::AllowAll)
        .build()
        .unwrap()
}

fn origins(list: Vec<Origin>) -> Origins {
    Origins::builder()
        .quantity(i32::try_from(list.len()).unwrap())
        .set_items(Some(list))
        .build()
        .unwrap()
}

fn config(enabled: bool, comment: &str, list: Vec<Origin>) -> DistributionConfig {
    let target = list[0].id().to_owned();
    DistributionConfig::builder()
        .caller_reference("ref-1")
        .comment(comment)
        .enabled(enabled)
        .origins(origins(list))
        .default_cache_behavior(behavior(&target))
        .price_class(PriceClass::PriceClassAll)
        .default_root_object("index.html")
        .build()
        .unwrap()
}

fn summary(id: &str, domain: &str, enabled: bool) -> DistributionSummary {
    DistributionSummary::builder()
        .id(id)
        .arn(format!(
            "arn:aws:cloudfront::123456789012:distribution/{id}"
        ))
        .status("Deployed")
        .last_modified_time(DateTime::from_secs(1_760_000_000))
        .domain_name(format!("{}.cloudfront.net", id.to_lowercase()))
        .aliases(Aliases::builder().quantity(0).build().unwrap())
        .origins(origins(vec![origin("o1", domain)]))
        .default_cache_behavior(behavior("o1"))
        .cache_behaviors(CacheBehaviors::builder().quantity(0).build().unwrap())
        .custom_error_responses(CustomErrorResponses::builder().quantity(0).build().unwrap())
        .comment(format!("comment of {id}"))
        .price_class(PriceClass::PriceClass100)
        .enabled(enabled)
        .viewer_certificate(ViewerCertificate::builder().build())
        .restrictions(
            Restrictions::builder()
                .geo_restriction(
                    GeoRestriction::builder()
                        .restriction_type(GeoRestrictionType::None)
                        .quantity(0)
                        .build()
                        .unwrap(),
                )
                .build(),
        )
        .web_acl_id("")
        .http_version(HttpVersion::Http2)
        .is_ipv6_enabled(true)
        .staging(false)
        .build()
        .unwrap()
}

fn page(items: Vec<DistributionSummary>, next: Option<&str>) -> ListDistributionsOutput {
    let mut list = DistributionList::builder()
        .marker("")
        .max_items(100)
        .is_truncated(next.is_some())
        .quantity(i32::try_from(items.len()).unwrap())
        .set_items(Some(items));
    if let Some(next) = next {
        list = list.next_marker(next);
    }
    ListDistributionsOutput::builder()
        .distribution_list(list.build().unwrap())
        .build()
}

fn full(id: &str, config: DistributionConfig) -> Distribution {
    Distribution::builder()
        .id(id)
        .arn(format!(
            "arn:aws:cloudfront::123456789012:distribution/{id}"
        ))
        .status("InProgress")
        .last_modified_time(DateTime::from_secs(1_760_000_100))
        .in_progress_invalidation_batches(1)
        .domain_name("d111.cloudfront.net")
        .distribution_config(config)
        .build()
        .unwrap()
}

fn invalidation(id: &str, status: &str, paths: &[&str]) -> Invalidation {
    Invalidation::builder()
        .id(id)
        .status(status)
        .create_time(DateTime::from_secs(1_760_000_200))
        .invalidation_batch(
            InvalidationBatch::builder()
                .paths(
                    Paths::builder()
                        .quantity(i32::try_from(paths.len()).unwrap())
                        .set_items(Some(paths.iter().map(|p| (*p).to_owned()).collect()))
                        .build()
                        .unwrap(),
                )
                .caller_reference("c")
                .build()
                .unwrap(),
        )
        .build()
        .unwrap()
}

fn summary_of(id: &str, status: &str, secs: i64) -> InvalidationSummary {
    InvalidationSummary::builder()
        .id(id)
        .status(status)
        .create_time(DateTime::from_secs(secs))
        .build()
        .unwrap()
}

fn error_response(status: u16, code: &'static str) -> impl Fn() -> HttpResponse + Send + Sync {
    move || {
        let body = format!(
            "<ErrorResponse xmlns=\"http://cloudfront.amazonaws.com/doc/2020-05-31/\"><Error>\
             <Type>Sender</Type><Code>{code}</Code><Message>{code} happened</Message></Error>\
             <RequestId>req-1</RequestId></ErrorResponse>"
        );
        HttpResponse::new(StatusCode::try_from(status).unwrap(), SdkBody::from(body))
    }
}

type Updates = Arc<Mutex<Vec<(DistributionConfig, Option<String>)>>>;
type Batches = Arc<Mutex<Vec<Vec<String>>>>;

/// The scripted service: `EA` serves a bucket and another origin.
struct Service {
    rules: Vec<Rule>,
    updates: Updates,
    batches: Batches,
}

fn bucket_and_api() -> DistributionConfig {
    config(
        true,
        "my site",
        vec![
            origin("api", "api.example.org"),
            origin("s3-site", "site.s3.eu-west-1.amazonaws.com"),
        ],
    )
}

fn service() -> Service {
    let updates: Updates = Arc::default();
    let batches: Batches = Arc::default();
    let record = Arc::clone(&updates);
    let batch_record = Arc::clone(&batches);
    let rules = vec![
        mock!(Client::list_distributions)
            .match_requests(|input| input.marker().is_none())
            .then_output(|| {
                page(
                    vec![summary("EA", "site.s3.amazonaws.com", true)],
                    Some("M2"),
                )
            }),
        mock!(Client::list_distributions)
            .match_requests(|input| input.marker() == Some("M2"))
            .then_output(|| page(vec![summary("EB", "other.example.org", false)], None)),
        mock!(Client::get_distribution)
            .match_requests(|input| input.id() == Some("EA"))
            .then_output(|| {
                GetDistributionOutput::builder()
                    .distribution(full("EA", bucket_and_api()))
                    .e_tag(ETAG)
                    .build()
            }),
        mock!(Client::get_distribution)
            .match_requests(|input| input.id() == Some("ENEW"))
            .then_output(|| {
                GetDistributionOutput::builder()
                    .distribution(full(
                        "ENEW",
                        config(
                            true,
                            "created",
                            vec![origin("s3-site", "site.s3.eu-west-1.amazonaws.com")],
                        ),
                    ))
                    .e_tag("E-NEW")
                    .build()
            }),
        mock!(Client::get_distribution)
            .then_http_response(error_response(404, "NoSuchDistribution")),
        mock!(Client::get_distribution_config)
            .match_requests(|input| input.id() == Some("EA"))
            .then_output(|| {
                GetDistributionConfigOutput::builder()
                    .distribution_config(bucket_and_api())
                    .e_tag(ETAG)
                    .build()
            }),
        mock!(Client::update_distribution)
            .match_requests(move |input| {
                record.lock().unwrap().push((
                    input.distribution_config().unwrap().clone(),
                    input.if_match().map(str::to_owned),
                ));
                true
            })
            .then_output(|| UpdateDistributionOutput::builder().e_tag("E2").build()),
        mock!(Client::create_origin_access_control).then_output(|| {
            CreateOriginAccessControlOutput::builder()
                .origin_access_control(OriginAccessControl::builder().id("OAC1").build().unwrap())
                .build()
        }),
        mock!(Client::create_distribution).then_output(|| {
            CreateDistributionOutput::builder()
                .distribution(full(
                    "ENEW",
                    config(
                        true,
                        "created",
                        vec![origin("s3-site", "x.s3.amazonaws.com")],
                    ),
                ))
                .e_tag("E-CREATED")
                .build()
        }),
        mock!(Client::create_invalidation)
            .match_requests(move |input| {
                let paths = input
                    .invalidation_batch()
                    .and_then(|batch| batch.paths())
                    .map(|paths| paths.items().to_vec())
                    .unwrap_or_default();
                batch_record.lock().unwrap().push(paths);
                true
            })
            .then_output(|| {
                CreateInvalidationOutput::builder()
                    .invalidation(invalidation("I1", "InProgress", &["/*"]))
                    .build()
            }),
        mock!(Client::list_invalidations)
            .match_requests(|input| input.marker().is_none())
            .then_output(|| {
                ListInvalidationsOutput::builder()
                    .invalidation_list(
                        InvalidationList::builder()
                            .marker("")
                            .max_items(50)
                            .is_truncated(true)
                            .next_marker("I1")
                            .quantity(1)
                            .items(summary_of("I2", "InProgress", 1_760_000_300))
                            .build()
                            .unwrap(),
                    )
                    .build()
            }),
        mock!(Client::list_invalidations)
            .match_requests(|input| input.marker() == Some("I1"))
            .then_output(|| {
                ListInvalidationsOutput::builder()
                    .invalidation_list(
                        InvalidationList::builder()
                            .marker("I1")
                            .max_items(50)
                            .is_truncated(false)
                            .quantity(1)
                            .items(summary_of("I1", "Completed", 1_760_000_200))
                            .build()
                            .unwrap(),
                    )
                    .build()
            }),
        mock!(Client::get_invalidation).then_output(|| {
            GetInvalidationOutput::builder()
                .invalidation(invalidation("I1", "Completed", &["/a/*", "/b.html"]))
                .build()
        }),
    ];
    Service {
        rules,
        updates,
        batches,
    }
}

/// Runs `bb args` in-process with the `environment` account and the
/// scripted CloudFront.
async fn run_cf(service: &Service, args: &[&str]) -> Output {
    run_cf_with(service, args, &[]).await
}

fn bb_with(service: &Service) -> (InProcess, Secrets) {
    let bb = InProcess::default();
    let rules = Arc::new(service.rules.clone());
    let secrets = Secrets::Store {
        store: bb.store.clone(),
        kdf_cost: InProcess::KDF,
        iam_config_hook: None,
        options_hook: None,
        cloudfront_config_hook: Some(Arc::new(move |builder| {
            let mut interceptor = MockResponseInterceptor::new().rule_mode(RuleMode::MatchAny);
            for rule in rules.iter() {
                interceptor = interceptor.with_rule(rule);
            }
            builder
                .interceptor(interceptor)
                .http_client(create_mock_http_client())
        })),
    };
    (bb, secrets)
}

async fn run_cf_with(service: &Service, args: &[&str], extra: &[(&str, &str)]) -> Output {
    let (bb, secrets) = bb_with(service);
    let mut env: Vec<(String, std::ffi::OsString)> = vec![
        ("BB_CONFIG_DIR".into(), bb.config.path().as_os_str().into()),
        ("BB_ACCESS_KEY_ID".into(), "AKIDEXAMPLE".into()),
        ("BB_SECRET_ACCESS_KEY".into(), "example-secret".into()),
    ];
    env.extend(extra.iter().map(|(k, v)| ((*k).into(), (*v).into())));
    run_with_secrets(secrets, env, args).await
}

async fn run_with_secrets(
    secrets: Secrets,
    env: Vec<(String, std::ffi::OsString)>,
    args: &[&str],
) -> Output {
    let stdout = Captured::default();
    let stderr = Captured::default();
    let io = Io {
        env: bucketbridge_cli::Env::from_pairs(env),
        stdin: Box::new(io::Cursor::new(Vec::new())),
        stdout: Box::new(stdout.clone()),
        stderr: Box::new(stderr.clone()),
        console: Box::new(Terminal::default()),
    };
    let args = std::iter::once("bb").chain(args.iter().copied());
    let exit = Box::pin(bucketbridge_cli::run(
        args,
        io,
        secrets,
        std::future::pending::<()>,
    ))
    .await;
    Output {
        code: i32::from(exit.code),
        stdout: stdout.text(),
        stderr: stderr.text(),
    }
}

#[tokio::test]
async fn list_walks_every_page() {
    let service = service();
    let out = run_cf(&service, &["cloudfront", "list"]).await;
    out.expect_code(0);
    assert_eq!(
        out.lines(),
        [
            "EA\tDeployed\tenabled\tea.cloudfront.net\tcomment of EA",
            "EB\tDeployed\tdisabled\teb.cloudfront.net\tcomment of EB",
        ]
    );
    let out = run_cf(&service, &["cloudfront", "list", "--json"]).await;
    out.expect_code(0);
    let records = out.records();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["id"], "EA");
    assert_eq!(records[0]["enabled"], true);
    assert_eq!(records[1]["enabled"], false);
    assert_eq!(records[0]["origins"][0]["bucket"], "site");
}

#[tokio::test]
async fn list_filters_by_bucket() {
    let service = service();
    let out = run_cf(&service, &["cloudfront", "list", "--bucket", "site"]).await;
    out.expect_code(0);
    assert_eq!(out.lines().len(), 1);
    assert!(out.lines()[0].starts_with("EA\t"));
    let out = run_cf(&service, &["cloudfront", "list", "--bucket", "nobody"]).await;
    out.expect_code(0);
    assert_eq!(out.stdout, "");
}

#[tokio::test]
async fn show_prints_the_details() {
    let service = service();
    let out = run_cf(&service, &["cloudfront", "show", "EA"]).await;
    out.expect_code(0);
    let text = out.stdout;
    assert!(
        text.contains(&format!(
            "{}\n",
            trf("domain: {domain}", &[("domain", "d111.cloudfront.net")])
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!("{}\n", trf("id: {id}", &[("id", "EA")]))),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "{}\n",
            trf("state: {state}", &[("state", "enabled")])
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "{}\n",
            trf("default root object: {root}", &[("root", "index.html")])
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "{}\n",
            trf("invalidations in progress: {count}", &[("count", "1")])
        )),
        "{text}"
    );
    assert!(
        text.contains(&trf(
            "origin {id}: {domain} (bucket {bucket})",
            &[
                ("id", "s3-site"),
                ("domain", "site.s3.eu-west-1.amazonaws.com"),
                ("bucket", "site"),
            ],
        )),
        "{text}"
    );
    // The website-endpoint and access-control suffixes are separate
    // fragments joined after the bucket.
    assert!(
        text.contains(&trf(
            "origin {id}: {domain}",
            &[("id", "api"), ("domain", "api.example.org")]
        )),
        "{text}"
    );
    let out = run_cf(&service, &["cloudfront", "show", "EA", "--json"]).await;
    out.expect_code(0);
    assert_eq!(out.records()[0]["etag"], ETAG);
}

#[tokio::test]
async fn an_unknown_distribution_is_not_found() {
    let service = service();
    let out = run_cf(&service, &["cloudfront", "show", "ENOPE"]).await;
    out.expect_code(2);
    assert!(out.stderr.starts_with("bb: error: "), "{}", out.stderr);
    let out = run_cf(&service, &["cloudfront", "show", "ENOPE", "--json"]).await;
    out.expect_code(2);
    assert_eq!(out.error_record()["code"], "not_found");
}

#[tokio::test]
async fn disable_and_enable_send_the_etag() {
    let service = service();
    let out = run_cf(&service, &["cloudfront", "disable", "EA"]).await;
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        trf("disabled distribution {id}", &[("id", "EA")])
    );
    let out = run_cf(&service, &["cloudfront", "enable", "EA", "--json"]).await;
    out.expect_code(0);
    assert_eq!(out.records()[0]["id"], "EA");
    let updates = service.updates.lock().unwrap();
    assert_eq!(updates.len(), 2);
    assert!(!updates[0].0.enabled());
    assert_eq!(updates[0].1.as_deref(), Some(ETAG));
    // The configuration read already is enabled: the edit is still sent.
    assert!(updates[1].0.enabled());
}

#[tokio::test]
async fn quiet_hides_the_outcome_of_a_change() {
    let service = service();
    let out = run_cf(&service, &["--quiet", "cloudfront", "disable", "EA"]).await;
    out.expect_code(0);
    assert_eq!(out.stdout, "");
}

#[tokio::test]
async fn detach_removes_the_origins_of_the_bucket() {
    let service = service();
    let out = run_cf(
        &service,
        &["cloudfront", "detach", "EA", "--bucket", "site"],
    )
    .await;
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        trf(
            "distribution {id} no longer serves bucket {bucket}",
            &[("id", "EA"), ("bucket", "site")]
        )
    );
    let updates = service.updates.lock().unwrap();
    let ids: Vec<&str> = updates[0]
        .0
        .origins()
        .unwrap()
        .items()
        .iter()
        .map(Origin::id)
        .collect();
    assert_eq!(ids, ["api"]);
}

#[tokio::test]
async fn invalidate_sends_the_paths() {
    let service = service();
    let out = run_cf(
        &service,
        &["cloudfront", "invalidate", "EA", "/a/*", "/b.html", "/a/*"],
    )
    .await;
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        trf(
            "created invalidation {id} ({status}): {paths}",
            &[("id", "I1"), ("status", "InProgress"), ("paths", "/*")]
        )
    );
    // Paths are trimmed and repeats removed before they are sent.
    assert_eq!(
        *service.batches.lock().unwrap(),
        [vec!["/a/*".to_owned(), "/b.html".to_owned()]]
    );
    let out = run_cf(
        &service,
        &["cloudfront", "invalidate", "EA", "/x", "--json"],
    )
    .await;
    out.expect_code(0);
    assert_eq!(out.records()[0]["id"], "I1");
}

#[tokio::test]
async fn bad_paths_are_usage_errors_and_send_nothing() {
    let service = service();
    for paths in [&["no-slash"][..], &["/a*b"], &["/with space"]] {
        let mut args = vec!["cloudfront", "invalidate", "EA"];
        args.extend_from_slice(paths);
        let out = run_cf(&service, &args).await;
        out.expect_code(1);
    }
    assert!(service.batches.lock().unwrap().is_empty());
    // At least one path is required (clap usage error).
    run_cf(&service, &["cloudfront", "invalidate", "EA"])
        .await
        .expect_code(1);
}

#[tokio::test]
async fn invalidation_history_and_details() {
    let service = service();
    let out = run_cf(&service, &["cloudfront", "invalidations", "EA"]).await;
    out.expect_code(0);
    assert_eq!(
        out.lines(),
        [
            "I2\tInProgress\t2025-10-09T08:58:20Z",
            "I1\tCompleted\t2025-10-09T08:56:40Z",
        ]
    );
    let out = run_cf(&service, &["cloudfront", "invalidations", "EA", "--json"]).await;
    out.expect_code(0);
    assert_eq!(out.records().len(), 2);
    let out = run_cf(&service, &["cloudfront", "invalidation", "EA", "I1"]).await;
    out.expect_code(0);
    assert_eq!(
        out.stdout.trim(),
        "I1\tCompleted\t2025-10-09T08:56:40Z\t/a/* /b.html"
    );
    let out = run_cf(
        &service,
        &["cloudfront", "invalidation", "EA", "I1", "--json"],
    )
    .await;
    out.expect_code(0);
    assert_eq!(out.records()[0]["paths"], json!(["/a/*", "/b.html"]));
}

#[tokio::test]
async fn create_prints_the_bucket_policy() {
    let service = service();
    let out = run_cf(
        &service,
        &[
            "cloudfront",
            "create",
            "--bucket",
            "site",
            "--region",
            "eu-west-1",
            "--comment",
            "created",
        ],
    )
    .await;
    out.expect_code(0);
    assert!(
        out.stdout.starts_with(&trf(
            "created distribution {id} ({domain}) for bucket {bucket}\n",
            &[
                ("id", "ENEW"),
                ("domain", "d111.cloudfront.net"),
                ("bucket", "site"),
            ],
        )),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("cloudfront.amazonaws.com"),
        "{}",
        out.stdout
    );
    let out = run_cf(
        &service,
        &[
            "cloudfront",
            "create",
            "--bucket",
            "site",
            "--region",
            "eu-west-1",
            "--json",
        ],
    )
    .await;
    out.expect_code(0);
    let record = &out.records()[0];
    assert_eq!(record["distribution"]["id"], "ENEW");
    assert_eq!(record["origin_access_control_id"], "OAC1");
    let policy: serde_json::Value =
        serde_json::from_str(record["bucket_policy"].as_str().unwrap()).unwrap();
    assert!(policy["Statement"].is_array());
}

#[tokio::test]
async fn an_account_with_a_custom_endpoint_has_no_cloudfront() {
    let service = service();
    let out = run_cf_with(
        &service,
        &["cloudfront", "list", "--json"],
        &[("BB_ENDPOINT", "http://127.0.0.1:9000")],
    )
    .await;
    out.expect_code(1);
    let record = out.error_record();
    assert_eq!(record["code"], "usage");
    assert!(
        record["message"]
            .as_str()
            .unwrap()
            .contains("custom endpoint"),
        "{record}"
    );
}

#[tokio::test]
async fn no_credentials_is_an_auth_error() {
    let service = service();
    let (bb, secrets) = bb_with(&service);
    let env = vec![(
        "BB_CONFIG_DIR".to_owned(),
        bb.config.path().as_os_str().into(),
    )];
    let out = run_with_secrets(secrets, env, &["cloudfront", "list"]).await;
    out.expect_code(3);
}

#[tokio::test]
async fn a_stored_account_works_too() {
    let service = service();
    let (bb, secrets) = bb_with(&service);
    bb.run_with(
        &[
            "account",
            "add",
            "--label",
            "AWS",
            "--access-key-id",
            "AKIDSTORED",
            "--region",
            "eu-west-1",
            "--secret-stdin",
        ],
        &[],
        b"stored-secret\n",
        &Terminal::default(),
    )
    .await
    .expect_code(0);
    let env = vec![(
        "BB_CONFIG_DIR".to_owned(),
        bb.config.path().as_os_str().into(),
    )];
    let out = run_with_secrets(
        secrets,
        env,
        &["--account", "aws", "cloudfront", "show", "EA", "--json"],
    )
    .await;
    out.expect_code(0);
    assert_eq!(out.records()[0]["id"], "EA");
}
