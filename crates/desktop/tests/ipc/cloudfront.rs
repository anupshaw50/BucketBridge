//! The CloudFront commands: contract shapes, error codes and what reaches
//! the service. CloudFront is replaced by scripted answers
//! (`aws-smithy-mocks`) injected through the session options; accounts with
//! a custom endpoint must be refused before anything is built.

use std::sync::{Arc, Mutex};

use aws_smithy_mocks::{MockResponseInterceptor, Rule, RuleMode, create_mock_http_client, mock};
use aws_smithy_runtime_api::client::orchestrator::HttpResponse;
use aws_smithy_runtime_api::http::StatusCode;
use aws_smithy_types::body::SdkBody;
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
use serde_json::{Value, json};

use crate::harness::*;

const ETAG: &str = "E-CURRENT";

const DISTRIBUTION_KEYS: [&str; 13] = [
    "aliases",
    "arn",
    "comment",
    "default_root_object",
    "domain_name",
    "enabled",
    "etag",
    "id",
    "in_progress_invalidations",
    "last_modified",
    "origins",
    "price_class",
    "status",
];

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

/// Configurations and ETags of the updates that reached the service.
type Updates = Arc<Mutex<Vec<(DistributionConfig, Option<String>)>>>;
type Batches = Arc<Mutex<Vec<Vec<String>>>>;

/// The scripted service: `EA` serves a bucket and one other origin.
struct Service {
    rules: Vec<Rule>,
    updates: Updates,
    batches: Batches,
}

fn service() -> Service {
    let updates: Updates = Arc::default();
    let batches: Batches = Arc::default();
    let bucket_and_api = || {
        config(
            true,
            "my site",
            vec![
                origin("api", "api.example.org"),
                origin("s3-site", "site.s3.eu-west-1.amazonaws.com"),
            ],
        )
    };
    let record = Arc::clone(&updates);
    let batch_record = Arc::clone(&batches);
    let rules = vec![
        mock!(bucketbridge_core::aws_sdk_cloudfront::Client::list_distributions)
            .match_requests(|input| input.marker().is_none())
            .then_output(|| {
                page(
                    vec![summary("EA", "site.s3.amazonaws.com", true)],
                    Some("M2"),
                )
            }),
        mock!(bucketbridge_core::aws_sdk_cloudfront::Client::list_distributions)
            .match_requests(|input| input.marker() == Some("M2"))
            .then_output(|| page(vec![summary("EB", "other.example.org", false)], None)),
        mock!(bucketbridge_core::aws_sdk_cloudfront::Client::get_distribution)
            .match_requests(|input| input.id() == Some("EA"))
            .then_output(move || {
                GetDistributionOutput::builder()
                    .distribution(full("EA", bucket_and_api()))
                    .e_tag(ETAG)
                    .build()
            }),
        mock!(bucketbridge_core::aws_sdk_cloudfront::Client::get_distribution)
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
        mock!(bucketbridge_core::aws_sdk_cloudfront::Client::get_distribution)
            .then_http_response(error_response(404, "NoSuchDistribution")),
        mock!(bucketbridge_core::aws_sdk_cloudfront::Client::get_distribution_config)
            .match_requests(|input| input.id() == Some("EA"))
            .then_output(|| {
                GetDistributionConfigOutput::builder()
                    .distribution_config(config(
                        true,
                        "my site",
                        vec![
                            origin("api", "api.example.org"),
                            origin("s3-site", "site.s3.eu-west-1.amazonaws.com"),
                        ],
                    ))
                    .e_tag(ETAG)
                    .build()
            }),
        mock!(bucketbridge_core::aws_sdk_cloudfront::Client::update_distribution)
            .match_requests(move |input| {
                record.lock().unwrap().push((
                    input.distribution_config().unwrap().clone(),
                    input.if_match().map(str::to_owned),
                ));
                true
            })
            .then_output(|| UpdateDistributionOutput::builder().e_tag("E2").build()),
        mock!(bucketbridge_core::aws_sdk_cloudfront::Client::create_origin_access_control)
            .then_output(|| {
                CreateOriginAccessControlOutput::builder()
                    .origin_access_control(
                        OriginAccessControl::builder().id("OAC1").build().unwrap(),
                    )
                    .build()
            }),
        mock!(bucketbridge_core::aws_sdk_cloudfront::Client::create_distribution).then_output(
            || {
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
            },
        ),
        mock!(bucketbridge_core::aws_sdk_cloudfront::Client::create_invalidation)
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
        mock!(bucketbridge_core::aws_sdk_cloudfront::Client::list_invalidations)
            .match_requests(|input| input.marker().is_none())
            .then_output(|| {
                ListInvalidationsOutput::builder()
                    .invalidation_list(
                        InvalidationList::builder()
                            .marker("")
                            .max_items(50)
                            .is_truncated(true)
                            .next_marker("I1")
                            .quantity(2)
                            .items(summary_of("I2", "InProgress", 1_760_000_300))
                            .items(summary_of("I1", "Completed", 1_760_000_200))
                            .build()
                            .unwrap(),
                    )
                    .build()
            }),
        mock!(bucketbridge_core::aws_sdk_cloudfront::Client::get_invalidation).then_output(|| {
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

fn summary_of(id: &str, status: &str, secs: i64) -> InvalidationSummary {
    InvalidationSummary::builder()
        .id(id)
        .status(status)
        .create_time(DateTime::from_secs(secs))
        .build()
        .unwrap()
}

/// An app whose CloudFront answers like `service`, with an AWS account.
fn app(service: &Service) -> (Harness, String) {
    let rules = Arc::new(service.rules.clone());
    let h = Harness::with_options(move |options| {
        options.cloudfront_config_hook = Some(Arc::new(move |builder| {
            let mut interceptor = MockResponseInterceptor::new().rule_mode(RuleMode::MatchAny);
            for rule in rules.iter() {
                interceptor = interceptor.with_rule(rule);
            }
            builder
                .interceptor(interceptor)
                .http_client(create_mock_http_client())
        }));
    });
    let account = h.ok(
        "add_account",
        json!({ "input": {
            "label": "AWS",
            "access_key_id": "AKIAEXAMPLE",
            "secret_access_key": "secret-value",
            "session_token": null,
            "region": "eu-west-1",
            "endpoint": null,
            "path_style": false,
            "request_payer": false,
            "mfa_serial": null,
        }}),
    );
    let id = account["id"].as_str().unwrap().to_owned();
    (h, id)
}

#[test]
fn accounts_with_a_custom_endpoint_have_no_cloudfront() {
    let server = start_s3();
    let h = Harness::new();
    let id = h.add_account(&server, "Custom");
    let calls = [
        (
            "list_cloudfront_distributions",
            json!({ "account_id": id, "marker": null }),
        ),
        (
            "get_cloudfront_distribution",
            json!({ "account_id": id, "id": "EA" }),
        ),
        (
            "find_cloudfront_distributions",
            json!({ "account_id": id, "bucket": "b" }),
        ),
        (
            "create_cloudfront_invalidation",
            json!({ "account_id": id, "distribution_id": "EA", "paths": ["/*"] }),
        ),
        (
            "list_cloudfront_invalidations",
            json!({ "account_id": id, "distribution_id": "EA", "marker": null }),
        ),
    ];
    for (command, args) in calls {
        let (code, message) = h.error(command, args);
        assert_eq!(code, "usage", "{command}");
        assert!(message.contains("custom endpoint"), "{command}: {message}");
    }
}

#[test]
fn an_unknown_account_is_not_found() {
    let h = Harness::new();
    let code = h.code(
        "list_cloudfront_distributions",
        json!({ "account_id": "00000000-0000-4000-8000-000000000000", "marker": null }),
    );
    assert_eq!(code, "not_found");
}

#[test]
fn distributions_are_listed_page_by_page_and_read() {
    let service = service();
    let (h, id) = app(&service);
    let first = h.ok(
        "list_cloudfront_distributions",
        json!({ "account_id": id, "marker": null }),
    );
    assert_eq!(keys(&first), ["items", "next_marker"]);
    assert_eq!(first["next_marker"], "M2");
    let item = &first["items"][0];
    assert_eq!(keys(item), DISTRIBUTION_KEYS);
    assert_eq!(item["id"], "EA");
    assert_eq!(item["etag"], Value::Null);
    assert_eq!(item["origins"][0]["bucket"], "site");
    assert_eq!(
        keys(&item["origins"][0]),
        [
            "access_control",
            "bucket",
            "domain_name",
            "id",
            "origin_path",
            "website"
        ]
    );
    let second = h.ok(
        "list_cloudfront_distributions",
        json!({ "account_id": id, "marker": "M2" }),
    );
    assert_eq!(second["next_marker"], Value::Null);
    assert_eq!(second["items"][0]["enabled"], false);

    let details = h.ok(
        "get_cloudfront_distribution",
        json!({ "account_id": id, "id": "EA" }),
    );
    assert_eq!(keys(&details), DISTRIBUTION_KEYS);
    assert_eq!(details["etag"], ETAG);
    assert_eq!(details["default_root_object"], "index.html");
    assert_eq!(details["in_progress_invalidations"], 1);
    assert_eq!(
        h.code(
            "get_cloudfront_distribution",
            json!({ "account_id": id, "id": "EZ" })
        ),
        "not_found"
    );
    assert_eq!(
        h.code(
            "get_cloudfront_distribution",
            json!({ "account_id": id, "id": "../x" })
        ),
        "usage"
    );
}

#[test]
fn the_distributions_of_a_bucket_are_found() {
    let service = service();
    let (h, id) = app(&service);
    let found = h.ok(
        "find_cloudfront_distributions",
        json!({ "account_id": id, "bucket": "site" }),
    );
    let ids: Vec<&str> = found
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["EA"]);
    let none = h.ok(
        "find_cloudfront_distributions",
        json!({ "account_id": id, "bucket": "nothing" }),
    );
    assert_eq!(none, json!([]));
}

#[test]
fn a_distribution_is_created_for_a_bucket() {
    let service = service();
    let (h, id) = app(&service);
    let created = h.ok(
        "create_cloudfront_distribution",
        json!({ "account_id": id, "new": {
            "bucket": "site", "region": "eu-west-1", "comment": "created",
            "enabled": true, "default_root_object": "index.html", "price_class": null
        }}),
    );
    assert_eq!(
        keys(&created),
        ["bucket_policy", "distribution", "origin_access_control_id"]
    );
    assert_eq!(created["origin_access_control_id"], "OAC1");
    assert_eq!(created["distribution"]["id"], "ENEW");
    assert!(
        created["bucket_policy"]
            .as_str()
            .unwrap()
            .contains("arn:aws:s3:::site/*")
    );
    assert_eq!(
        h.code(
            "create_cloudfront_distribution",
            json!({ "account_id": id, "new": {
                "bucket": "site", "region": "eu-west-1", "comment": "x".repeat(200),
                "enabled": true, "default_root_object": "", "price_class": null
            }}),
        ),
        "usage"
    );
}

#[test]
fn edits_enable_disable_and_detach_send_the_etag() {
    let service = service();
    let (h, id) = app(&service);
    let edit = json!({
        "comment": "renamed", "aliases": ["www.example.com"],
        "default_root_object": null, "price_class": "PriceClass_200", "enabled": null
    });
    let updated = h.ok(
        "update_cloudfront_distribution",
        json!({ "account_id": id, "id": "EA", "edit": edit, "expected_etag": ETAG }),
    );
    assert_eq!(keys(&updated), DISTRIBUTION_KEYS);
    h.ok(
        "set_cloudfront_enabled",
        json!({ "account_id": id, "id": "EA", "enabled": false, "expected_etag": null }),
    );
    // The default behaviour uses "api", so the bucket origin can go.
    h.ok(
        "detach_cloudfront_bucket",
        json!({ "account_id": id, "id": "EA", "bucket": "site", "expected_etag": ETAG }),
    );
    let updates = service.updates.lock().unwrap();
    assert_eq!(updates.len(), 3);
    for (_, if_match) in updates.iter() {
        assert_eq!(if_match.as_deref(), Some(ETAG));
    }
    assert_eq!(updates[0].0.comment(), "renamed");
    assert_eq!(updates[0].0.aliases().unwrap().items(), ["www.example.com"]);
    assert!(!updates[1].0.enabled());
    assert_eq!(updates[2].0.origins().unwrap().quantity(), 1);
    drop(updates);

    // A stale ETag is a conflict and sends nothing.
    let code = h.code(
        "set_cloudfront_enabled",
        json!({ "account_id": id, "id": "EA", "enabled": true, "expected_etag": "E-OLD" }),
    );
    assert_eq!(code, "conflict");
    assert_eq!(service.updates.lock().unwrap().len(), 3);
    assert_eq!(
        h.code(
            "update_cloudfront_distribution",
            json!({ "account_id": id, "id": "EA",
                "edit": { "aliases": ["not a domain"] }, "expected_etag": null }),
        ),
        "usage"
    );
}

#[test]
fn invalidations_are_created_listed_and_read() {
    let service = service();
    let (h, id) = app(&service);
    let created = h.ok(
        "create_cloudfront_invalidation",
        json!({ "account_id": id, "distribution_id": "EA", "paths": ["/*", " /a/*", "/a/*"] }),
    );
    assert_eq!(keys(&created), ["create_time", "id", "paths", "status"]);
    assert_eq!(created["status"], "InProgress");
    assert_eq!(
        service.batches.lock().unwrap().as_slice(),
        [vec!["/*".to_owned(), "/a/*".to_owned()]]
    );

    for bad in [json!(["/a*b"]), json!(["a"]), json!([])] {
        assert_eq!(
            h.code(
                "create_cloudfront_invalidation",
                json!({ "account_id": id, "distribution_id": "EA", "paths": bad }),
            ),
            "usage"
        );
    }
    assert_eq!(service.batches.lock().unwrap().len(), 1);

    let history = h.ok(
        "list_cloudfront_invalidations",
        json!({ "account_id": id, "distribution_id": "EA", "marker": null }),
    );
    assert_eq!(keys(&history), ["items", "next_marker"]);
    assert_eq!(history["next_marker"], "I1");
    assert_eq!(history["items"][0]["status"], "InProgress");
    assert_eq!(history["items"][1]["status"], "Completed");
    assert_eq!(history["items"][0]["paths"], json!([]));

    let one = h.ok(
        "get_cloudfront_invalidation",
        json!({ "account_id": id, "distribution_id": "EA", "invalidation_id": "I1" }),
    );
    assert_eq!(one["paths"], json!(["/a/*", "/b.html"]));
}

#[test]
fn paths_are_validated_without_an_account() {
    let h = Harness::new();
    assert_eq!(
        h.ok(
            "validate_invalidation_paths",
            json!({ "paths": [" /a/* ", "/a/*", "/b"] })
        ),
        json!(["/a/*", "/b"])
    );
    let (code, message) = h.error(
        "validate_invalidation_paths",
        json!({ "paths": ["x", "/a*b"] }),
    );
    assert_eq!(code, "usage");
    assert!(message.contains("must start with '/'"), "{message}");
    assert!(message.contains("only allowed as the last"), "{message}");
}

#[test]
fn path_sets_are_saved_replaced_and_deleted() {
    let h = Harness::new();
    assert_eq!(h.ok("list_invalidation_path_sets", json!({})), json!([]));
    let saved = h.ok(
        "save_invalidation_path_set",
        json!({ "set": { "name": "Site", "paths": ["/*"] }, "replaces": null }),
    );
    assert_eq!(saved, json!([{ "name": "Site", "paths": ["/*"] }]));
    let saved = h.ok(
        "save_invalidation_path_set",
        json!({ "set": { "name": "Images", "paths": ["/img/*"] }, "replaces": null }),
    );
    assert_eq!(saved.as_array().unwrap().len(), 2);
    assert_eq!(
        h.code(
            "save_invalidation_path_set",
            json!({ "set": { "name": "site", "paths": ["/x"] }, "replaces": null }),
        ),
        "conflict"
    );
    assert_eq!(
        h.code(
            "save_invalidation_path_set",
            json!({ "set": { "name": "Bad", "paths": ["x"] }, "replaces": null }),
        ),
        "usage"
    );
    let replaced = h.ok(
        "save_invalidation_path_set",
        json!({ "set": { "name": "Site 2", "paths": ["/a", "/b"] }, "replaces": "site" }),
    );
    assert_eq!(
        replaced[0],
        json!({ "name": "Site 2", "paths": ["/a", "/b"] })
    );
    assert_eq!(h.ok("list_invalidation_path_sets", json!({})), replaced);
    let left = h.ok("delete_invalidation_path_set", json!({ "name": "IMAGES" }));
    assert_eq!(left.as_array().unwrap().len(), 1);
    assert_eq!(
        h.code("delete_invalidation_path_set", json!({ "name": "Images" })),
        "not_found"
    );
}
