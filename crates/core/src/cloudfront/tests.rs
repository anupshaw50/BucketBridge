use std::sync::{Arc, Mutex};

use aws_sdk_cloudfront::operation::create_distribution::CreateDistributionOutput;
use aws_sdk_cloudfront::operation::create_invalidation::CreateInvalidationOutput;
use aws_sdk_cloudfront::operation::create_origin_access_control::CreateOriginAccessControlOutput;
use aws_sdk_cloudfront::operation::get_distribution::GetDistributionOutput;
use aws_sdk_cloudfront::operation::get_distribution_config::GetDistributionConfigOutput;
use aws_sdk_cloudfront::operation::get_invalidation::GetInvalidationOutput;
use aws_sdk_cloudfront::operation::list_distributions::ListDistributionsOutput;
use aws_sdk_cloudfront::operation::list_invalidations::ListInvalidationsOutput;
use aws_sdk_cloudfront::operation::update_distribution::UpdateDistributionOutput;
use aws_sdk_cloudfront::types::{
    CacheBehavior, CacheBehaviors, DistributionList, HttpVersion, Invalidation as SdkInvalidation,
    InvalidationList, OriginAccessControl, OriginGroup, OriginGroupMember, OriginGroupMembers,
    OriginGroups,
};
use aws_smithy_mocks::{MockResponseInterceptor, Rule, RuleMode, create_mock_http_client, mock};
use aws_smithy_runtime_api::client::orchestrator::HttpResponse;
use aws_smithy_runtime_api::http::StatusCode;
use aws_smithy_types::DateTime as SmithyDateTime;
use aws_smithy_types::body::SdkBody;

use super::*;
use crate::error::ErrorKind;

const ETAG: &str = "E2QWRUHAPOMQZL";

fn account() -> Account {
    Account {
        id: "account-a".into(),
        region: "eu-west-1".into(),
        ..Account::default()
    }
}

fn creds() -> Credentials {
    Credentials::new("AKIAEXAMPLE", "secret", None)
}

fn client_with(rules: &[&Rule], mode: RuleMode) -> CloudFront {
    let mut interceptor = MockResponseInterceptor::new().rule_mode(mode);
    for rule in rules {
        interceptor = interceptor.with_rule(rule);
    }
    let config = cloudfront_config(&account(), &creds(), &ClientOptions::default())
        .unwrap()
        .interceptor(interceptor)
        .http_client(create_mock_http_client())
        .build();
    CloudFront::from_client(aws_sdk_cloudfront::Client::from_conf(config))
}

fn sequential(rules: &[&Rule]) -> CloudFront {
    client_with(rules, RuleMode::Sequential)
}

fn http_error(
    status: u16,
    code: &'static str,
) -> impl Fn() -> HttpResponse + Send + Sync + 'static {
    move || {
        let body = format!(
            "<ErrorResponse xmlns=\"http://cloudfront.amazonaws.com/doc/2020-05-31/\"><Error>\
             <Type>Sender</Type><Code>{code}</Code><Message>{code} happened</Message></Error>\
             <RequestId>req-1</RequestId></ErrorResponse>"
        );
        HttpResponse::new(StatusCode::try_from(status).unwrap(), SdkBody::from(body))
    }
}

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
        .quantity(list.len() as i32)
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
        .last_modified_time(SmithyDateTime::from_secs(1_760_000_000))
        .domain_name(format!("{}.cloudfront.net", id.to_lowercase()))
        .aliases(Aliases::builder().quantity(0).build().unwrap())
        .origins(origins(vec![origin("o1", domain)]))
        .default_cache_behavior(behavior("o1"))
        .cache_behaviors(CacheBehaviors::builder().quantity(0).build().unwrap())
        .custom_error_responses(
            aws_sdk_cloudfront::types::CustomErrorResponses::builder()
                .quantity(0)
                .build()
                .unwrap(),
        )
        .comment(format!("comment of {id}"))
        .price_class(PriceClass::PriceClass100)
        .enabled(enabled)
        .viewer_certificate(aws_sdk_cloudfront::types::ViewerCertificate::builder().build())
        .restrictions(
            aws_sdk_cloudfront::types::Restrictions::builder()
                .geo_restriction(
                    aws_sdk_cloudfront::types::GeoRestriction::builder()
                        .restriction_type(aws_sdk_cloudfront::types::GeoRestrictionType::None)
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
        .quantity(items.len() as i32)
        .set_items(Some(items));
    if let Some(next) = next {
        list = list.next_marker(next);
    }
    ListDistributionsOutput::builder()
        .distribution_list(list.build().unwrap())
        .build()
}

fn full_distribution(
    id: &str,
    config: DistributionConfig,
    in_progress: i32,
) -> aws_sdk_cloudfront::types::Distribution {
    aws_sdk_cloudfront::types::Distribution::builder()
        .id(id)
        .arn(format!(
            "arn:aws:cloudfront::123456789012:distribution/{id}"
        ))
        .status("InProgress")
        .last_modified_time(SmithyDateTime::from_secs(1_760_000_100))
        .in_progress_invalidation_batches(in_progress)
        .domain_name("d111.cloudfront.net")
        .distribution_config(config)
        .build()
        .unwrap()
}

fn get_rule(id: &'static str, config: DistributionConfig, etag: &'static str) -> Rule {
    mock!(aws_sdk_cloudfront::Client::get_distribution)
        .match_requests(move |input| input.id() == Some(id))
        .then_output(move || {
            GetDistributionOutput::builder()
                .distribution(full_distribution(id, config.clone(), 2))
                .e_tag(etag)
                .build()
        })
}

fn get_config_rule(id: &'static str, config: DistributionConfig, etag: &'static str) -> Rule {
    mock!(aws_sdk_cloudfront::Client::get_distribution_config)
        .match_requests(move |input| input.id() == Some(id))
        .then_output(move || {
            GetDistributionConfigOutput::builder()
                .distribution_config(config.clone())
                .e_tag(etag)
                .build()
        })
}

fn bucket_origin() -> Origin {
    origin("s3-site", "site.s3.eu-west-1.amazonaws.com")
}

fn other_origin() -> Origin {
    origin("api", "api.example.org")
}

/// Records the configuration and the `If-Match` of every update.
type Sent = Arc<Mutex<Vec<(DistributionConfig, Option<String>)>>>;

fn update_rule(sent: &Sent) -> Rule {
    let sent = Arc::clone(sent);
    mock!(aws_sdk_cloudfront::Client::update_distribution)
        .match_requests(move |input| {
            sent.lock().unwrap().push((
                input.distribution_config().unwrap().clone(),
                input.if_match().map(str::to_owned),
            ));
            true
        })
        .then_output(|| UpdateDistributionOutput::builder().e_tag("E-NEW").build())
}

// ---- paths ----

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn paths_accept_wildcards_at_the_end_only() {
    let ok = validate_paths(&strings(&["/*", "/images/*", "/a/b.html", " /trim "])).unwrap();
    assert_eq!(ok, ["/*", "/images/*", "/a/b.html", "/trim"]);
    for bad in [
        "/a*b", "/*/x", "/**x", "*", "images/a", "", "   ", "/a b", "/a\tb", "/a\nb",
    ] {
        let error = validate_paths(&strings(&[bad])).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Usage, "{bad:?}");
    }
}

#[test]
fn paths_report_every_problem_and_drop_repeats() {
    let error = validate_paths(&strings(&["a", "/b*c", "/ok"])).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("must start with '/'"), "{message}");
    assert!(message.contains("only allowed as the last"), "{message}");
    assert_eq!(
        validate_paths(&strings(&["/a", "/b", "/a"])).unwrap(),
        ["/a", "/b"]
    );
    assert_eq!(validate_paths(&[]).unwrap_err().kind(), ErrorKind::Usage);
}

#[test]
fn paths_have_limits() {
    let many: Vec<String> = (0..=MAX_INVALIDATION_PATHS)
        .map(|i| format!("/{i}"))
        .collect();
    assert_eq!(validate_paths(&many).unwrap_err().kind(), ErrorKind::Usage);
    assert!(validate_paths(&many[..MAX_INVALIDATION_PATHS]).is_ok());
    let long = format!("/{}", "a".repeat(MAX_PATH_BYTES));
    assert_eq!(
        validate_paths(&[long]).unwrap_err().kind(),
        ErrorKind::Usage
    );
    let exact = format!("/{}", "a".repeat(MAX_PATH_BYTES - 1));
    assert!(validate_paths(&[exact]).is_ok());
}

// ---- origins and buckets ----

#[test]
fn origin_domains_name_their_bucket() {
    let bucket = |domain: &str| origin_bucket(domain);
    assert_eq!(
        bucket("site.s3.amazonaws.com"),
        Some(("site".into(), false))
    );
    assert_eq!(
        bucket("site.s3.eu-west-1.amazonaws.com"),
        Some(("site".into(), false))
    );
    assert_eq!(
        bucket("site.s3-eu-west-1.amazonaws.com"),
        Some(("site".into(), false))
    );
    assert_eq!(
        bucket("Site.S3.Dualstack.us-east-1.amazonaws.com"),
        Some(("site".into(), false))
    );
    assert_eq!(
        bucket("site.s3-website-us-east-1.amazonaws.com"),
        Some(("site".into(), true))
    );
    assert_eq!(
        bucket("site.s3-website.eu-west-1.amazonaws.com"),
        Some(("site".into(), true))
    );
    assert_eq!(
        bucket("site.s3.cn-north-1.amazonaws.com.cn"),
        Some(("site".into(), false))
    );
    // Dots, even ".s3", in the bucket name.
    assert_eq!(
        bucket("my.s3.data.s3.us-east-1.amazonaws.com"),
        Some(("my.s3.data".into(), false))
    );
    assert_eq!(bucket("api.example.org"), None);
    assert_eq!(bucket("s3.amazonaws.com"), None);
    assert_eq!(bucket("site.s3.example.org"), None);
    assert_eq!(bucket("site.execute-api.eu-west-1.amazonaws.com"), None);
}

fn model(id: &str, domains: &[&str]) -> Distribution {
    Distribution {
        id: id.into(),
        arn: format!("arn:aws:cloudfront::1:distribution/{id}"),
        domain_name: "x.cloudfront.net".into(),
        status: "Deployed".into(),
        enabled: true,
        comment: String::new(),
        aliases: vec![],
        price_class: "PriceClass_All".into(),
        last_modified: None,
        origins: domains
            .iter()
            .enumerate()
            .map(|(i, domain)| origin_to_model(&origin(&format!("o{i}"), domain)))
            .collect(),
        default_root_object: None,
        in_progress_invalidations: None,
        etag: None,
    }
}

#[test]
fn distributions_are_matched_to_buckets() {
    let all = vec![
        model("A", &["site.s3.amazonaws.com"]),
        model(
            "B",
            &["api.example.org", "assets.s3.eu-west-1.amazonaws.com"],
        ),
        model("C", &["site.s3-website-us-east-1.amazonaws.com"]),
    ];
    let ids = |list: Vec<Distribution>| list.into_iter().map(|d| d.id).collect::<Vec<_>>();
    assert_eq!(ids(distributions_for_bucket(&all, "site")), ["A", "C"]);
    assert_eq!(ids(distributions_for_bucket(&all, "SITE")), ["A", "C"]);
    assert_eq!(ids(distributions_for_bucket(&all, "assets")), ["B"]);
    assert!(distributions_for_bucket(&all, "none").is_empty());
    let names = strings(&["site", "other", "assets"]);
    let marked = buckets_with_distributions(&all, &names);
    assert_eq!(marked.len(), 2);
    assert!(marked.contains("site") && marked.contains("assets"));
}

#[test]
fn bucket_policy_names_the_distribution_only() {
    let policy = oac_bucket_policy("site", "arn:aws:cloudfront::123456789012:distribution/EABC");
    let value: serde_json::Value = serde_json::from_str(&policy).unwrap();
    let statement = &value["Statement"][0];
    assert_eq!(statement["Resource"], "arn:aws:s3:::site/*");
    assert_eq!(statement["Action"], "s3:GetObject");
    assert_eq!(
        statement["Principal"]["Service"],
        "cloudfront.amazonaws.com"
    );
    assert_eq!(
        statement["Condition"]["StringEquals"]["AWS:SourceArn"],
        "arn:aws:cloudfront::123456789012:distribution/EABC"
    );
    let china = oac_bucket_policy("site", "arn:aws-cn:cloudfront::1:distribution/E");
    assert!(china.contains("arn:aws-cn:s3:::site/*"));
}

#[test]
fn aliases_must_be_domain_names() {
    for good in ["www.example.com", "*.example.com", "a-b.example.co.uk"] {
        assert!(check_alias(good).is_ok(), "{good}");
    }
    for bad in [
        "",
        "example",
        "-a.example.com",
        "a..example.com",
        "a b.example.com",
        "*.",
        "exa_mple.com",
    ] {
        assert!(check_alias(bad).is_err(), "{bad}");
    }
}

// ---- client ----

#[test]
fn custom_endpoint_accounts_have_no_cloudfront() {
    let mut external = account();
    external.endpoint = Some("https://s3.example.org".into());
    let error = CloudFront::new(&external, &creds(), &ClientOptions::default(), None).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Usage);
    assert!(error.to_string().contains("custom endpoint"));
    assert_eq!(
        check_account(&external).unwrap_err().kind(),
        ErrorKind::Usage
    );
}

#[test]
fn requests_are_signed_for_the_global_region() {
    assert_eq!(signing_region(&account()), "us-east-1");
    let mut china = account();
    china.region = "cn-north-1".into();
    assert_eq!(signing_region(&china), "cn-northwest-1");
    let config = cloudfront_config(&account(), &creds(), &ClientOptions::default())
        .unwrap()
        .build();
    assert_eq!(config.region().map(|r| r.as_ref()), Some("us-east-1"));
    let timeouts = config.timeout_config().unwrap();
    assert_eq!(
        timeouts.read_timeout(),
        Some(ClientOptions::default().read_timeout)
    );
}

// ---- listing ----

#[tokio::test]
async fn distributions_are_listed_across_pages() {
    let first = mock!(aws_sdk_cloudfront::Client::list_distributions)
        .match_requests(|input| input.marker().is_none())
        .then_output(|| {
            page(
                vec![summary("EA", "site.s3.amazonaws.com", true)],
                Some("MARK2"),
            )
        });
    let second = mock!(aws_sdk_cloudfront::Client::list_distributions)
        .match_requests(|input| input.marker() == Some("MARK2"))
        .then_output(|| page(vec![summary("EB", "api.example.org", false)], None));
    let cloudfront = sequential(&[&first, &second]);
    let all = cloudfront.list_all_distributions().await.unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(first.num_calls(), 1);
    assert_eq!(second.num_calls(), 1);
    let a = &all[0];
    assert_eq!(a.id, "EA");
    assert_eq!(a.status, "Deployed");
    assert!(a.enabled);
    assert_eq!(a.comment, "comment of EA");
    assert_eq!(a.price_class, "PriceClass_100");
    assert_eq!(a.domain_name, "ea.cloudfront.net");
    assert_eq!(a.origins[0].bucket.as_deref(), Some("site"));
    assert!(a.last_modified.is_some());
    assert_eq!(a.etag, None);
    assert!(!all[1].enabled);
    assert_eq!(all[1].origins[0].bucket, None);
}

#[tokio::test]
async fn a_repeated_marker_stops_the_listing() {
    let first = mock!(aws_sdk_cloudfront::Client::list_distributions)
        .match_requests(|input| input.marker().is_none())
        .then_output(|| page(vec![], Some("LOOP")));
    let looping = mock!(aws_sdk_cloudfront::Client::list_distributions)
        .match_requests(|input| input.marker() == Some("LOOP"))
        .then_output(|| page(vec![], Some("LOOP")));
    let cloudfront = client_with(&[&first, &looping], RuleMode::MatchAny);
    let error = cloudfront.list_all_distributions().await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Remote);
    assert!(error.to_string().contains("same marker twice"), "{error}");
    assert_eq!(first.num_calls(), 1);
    assert_eq!(looping.num_calls(), 1);
}

#[tokio::test]
async fn a_cycle_of_markers_stops_the_listing() {
    let rule = |from: Option<&'static str>, to: &'static str| {
        mock!(aws_sdk_cloudfront::Client::list_distributions)
            .match_requests(move |input| input.marker() == from)
            .then_output(move || page(vec![], Some(to)))
    };
    let (a, b, c) = (rule(None, "A"), rule(Some("A"), "B"), rule(Some("B"), "A"));
    let cloudfront = client_with(&[&a, &b, &c], RuleMode::MatchAny);
    let error = cloudfront.list_all_distributions().await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Remote);
}

#[tokio::test]
async fn an_empty_account_lists_nothing() {
    let rule = mock!(aws_sdk_cloudfront::Client::list_distributions)
        .then_output(|| ListDistributionsOutput::builder().build());
    let cloudfront = sequential(&[&rule]);
    assert!(
        cloudfront
            .list_all_distributions()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_page_reports_its_marker() {
    let rule = mock!(aws_sdk_cloudfront::Client::list_distributions)
        .match_requests(|input| input.max_items() == Some(PAGE_ITEMS))
        .then_output(|| page(vec![summary("EA", "x.example.org", true)], Some("NEXT")));
    let cloudfront = sequential(&[&rule]);
    let result = cloudfront.list_distributions_page(None).await.unwrap();
    assert_eq!(result.next_marker.as_deref(), Some("NEXT"));
    assert_eq!(result.items.len(), 1);
}

#[tokio::test]
async fn distributions_for_a_bucket_are_found_in_the_listing() {
    let rule = mock!(aws_sdk_cloudfront::Client::list_distributions).then_output(|| {
        page(
            vec![
                summary("EA", "site.s3.amazonaws.com", true),
                summary("EB", "other.s3.amazonaws.com", true),
            ],
            None,
        )
    });
    let cloudfront = sequential(&[&rule]);
    let found = cloudfront.distributions_for_bucket("site").await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "EA");
}

// ---- details ----

#[tokio::test]
async fn details_carry_the_etag_and_the_root_object() {
    let rule = get_rule("EA", config(true, "my site", vec![bucket_origin()]), ETAG);
    let cloudfront = sequential(&[&rule]);
    let details = cloudfront.get_distribution("EA").await.unwrap();
    assert_eq!(details.id, "EA");
    assert_eq!(details.etag.as_deref(), Some(ETAG));
    assert_eq!(details.default_root_object.as_deref(), Some("index.html"));
    assert_eq!(details.in_progress_invalidations, Some(2));
    assert_eq!(details.status, "InProgress");
    assert_eq!(details.comment, "my site");
    assert_eq!(details.origins[0].bucket.as_deref(), Some("site"));
}

#[tokio::test]
async fn an_unknown_distribution_is_not_found() {
    let rule = mock!(aws_sdk_cloudfront::Client::get_distribution)
        .then_http_response(http_error(404, "NoSuchDistribution"));
    let cloudfront = sequential(&[&rule]);
    let error = cloudfront.get_distribution("EZ").await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::NotFound);
    assert!(
        error.to_string().contains("reading distribution EZ"),
        "{error}"
    );
}

#[tokio::test]
async fn ids_are_checked_before_anything_is_sent() {
    let rule = mock!(aws_sdk_cloudfront::Client::get_distribution)
        .then_output(|| GetDistributionOutput::builder().build());
    let cloudfront = sequential(&[&rule]);
    for bad in ["", "E/../x", "a b"] {
        assert_eq!(
            cloudfront.get_distribution(bad).await.unwrap_err().kind(),
            ErrorKind::Usage
        );
    }
    assert_eq!(rule.num_calls(), 0);
}

#[tokio::test]
async fn access_denied_is_an_auth_error() {
    let rule = mock!(aws_sdk_cloudfront::Client::list_distributions)
        .then_http_response(http_error(403, "AccessDenied"));
    let cloudfront = sequential(&[&rule]);
    let error = cloudfront.list_all_distributions().await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Auth);
}

// ---- create ----

fn new_distribution() -> NewDistribution {
    NewDistribution {
        bucket: "site".into(),
        region: "eu-west-1".into(),
        comment: " my site ".into(),
        enabled: true,
        default_root_object: "index.html".into(),
        price_class: Some("PriceClass_100".into()),
    }
}

#[tokio::test]
async fn a_distribution_is_created_with_an_access_control() {
    let control = mock!(aws_sdk_cloudfront::Client::create_origin_access_control)
        .match_requests(|input| {
            let config = input.origin_access_control_config().unwrap();
            config.name().starts_with("bb-site-")
                && config.signing_behavior() == &OriginAccessControlSigningBehaviors::Always
                && config.signing_protocol() == &OriginAccessControlSigningProtocols::Sigv4
                && config.origin_access_control_origin_type() == &OriginAccessControlOriginTypes::S3
        })
        .then_output(|| {
            CreateOriginAccessControlOutput::builder()
                .origin_access_control(OriginAccessControl::builder().id("OAC1").build().unwrap())
                .build()
        });
    let seen: Arc<Mutex<Vec<DistributionConfig>>> = Arc::default();
    let record = Arc::clone(&seen);
    let create = mock!(aws_sdk_cloudfront::Client::create_distribution)
        .match_requests(move |input| {
            record
                .lock()
                .unwrap()
                .push(input.distribution_config().unwrap().clone());
            true
        })
        .then_output(|| {
            CreateDistributionOutput::builder()
                .distribution(full_distribution(
                    "ENEW",
                    config(true, "my site", vec![bucket_origin()]),
                    0,
                ))
                .e_tag("E-CREATED")
                .build()
        });
    let get = get_rule(
        "ENEW",
        config(true, "my site", vec![bucket_origin()]),
        "E-READ",
    );
    let cloudfront = sequential(&[&control, &create, &get]);
    let created = cloudfront
        .create_distribution(&new_distribution())
        .await
        .unwrap();
    assert_eq!(created.origin_access_control_id, "OAC1");
    assert_eq!(created.distribution.id, "ENEW");
    assert_eq!(created.distribution.etag.as_deref(), Some("E-READ"));
    assert!(created.bucket_policy.contains("arn:aws:s3:::site/*"));
    assert!(created.bucket_policy.contains("distribution/ENEW"));

    let sent = seen.lock().unwrap();
    let config = &sent[0];
    assert!(config.enabled());
    assert_eq!(config.comment(), "my site");
    assert_eq!(config.default_root_object(), Some("index.html"));
    assert_eq!(config.price_class(), Some(&PriceClass::PriceClass100));
    assert!(!config.caller_reference().is_empty());
    let origin = &config.origins().unwrap().items()[0];
    assert_eq!(origin.domain_name(), "site.s3.eu-west-1.amazonaws.com");
    assert_eq!(origin.origin_access_control_id(), Some("OAC1"));
    assert_eq!(
        origin.s3_origin_config().unwrap().origin_access_identity(),
        ""
    );
    let default = config.default_cache_behavior().unwrap();
    assert_eq!(default.target_origin_id(), origin.id());
    assert_eq!(
        default.viewer_protocol_policy(),
        &ViewerProtocolPolicy::RedirectToHttps
    );
    assert_eq!(default.compress(), Some(true));
    assert_eq!(default.cache_policy_id(), Some(CACHING_OPTIMIZED));
}

#[tokio::test]
async fn invalid_new_distributions_send_nothing() {
    let control = mock!(aws_sdk_cloudfront::Client::create_origin_access_control)
        .then_output(|| CreateOriginAccessControlOutput::builder().build());
    let cloudfront = client_with(&[&control], RuleMode::MatchAny);
    let bad = |change: fn(&mut NewDistribution)| {
        let mut new = new_distribution();
        change(&mut new);
        new
    };
    for new in [
        bad(|n| n.bucket = "Not A Bucket".into()),
        bad(|n| n.region = "not a region".into()),
        bad(|n| n.comment = "x".repeat(MAX_COMMENT_CHARS + 1)),
        bad(|n| n.default_root_object = "/index.html".into()),
        bad(|n| n.price_class = Some("Cheapest".into())),
    ] {
        let error = cloudfront.create_distribution(&new).await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Usage, "{new:?}");
    }
    assert_eq!(control.num_calls(), 0);
}

#[tokio::test]
async fn china_buckets_use_the_china_endpoint_suffix() {
    let mut new = new_distribution();
    new.region = "cn-north-1".into();
    let plan = validate_new(&new).unwrap();
    assert_eq!(plan.domain, "site.s3.cn-north-1.amazonaws.com.cn");
    assert_eq!(plan.comment, "my site");
}

#[tokio::test]
async fn a_failed_create_is_reported_with_its_context() {
    let control =
        mock!(aws_sdk_cloudfront::Client::create_origin_access_control).then_output(|| {
            CreateOriginAccessControlOutput::builder()
                .origin_access_control(OriginAccessControl::builder().id("OAC1").build().unwrap())
                .build()
        });
    let create = mock!(aws_sdk_cloudfront::Client::create_distribution)
        .then_http_response(http_error(409, "CNAMEAlreadyExists"));
    let cloudfront = sequential(&[&control, &create]);
    let error = cloudfront
        .create_distribution(&new_distribution())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Conflict);
    assert!(
        error
            .to_string()
            .contains("creating a distribution for 'site'"),
        "{error}"
    );
}

// ---- edit ----

#[tokio::test]
async fn an_edit_changes_only_what_it_names_and_sends_the_etag() {
    let original = config(true, "old", vec![bucket_origin(), other_origin()]);
    let get_config = get_config_rule("EA", original.clone(), ETAG);
    let sent: Sent = Arc::default();
    let update = update_rule(&sent);
    let get = get_rule("EA", config(true, "new", vec![bucket_origin()]), "E-NEW");
    let cloudfront = sequential(&[&get_config, &update, &get]);
    let edit = DistributionEdit {
        comment: Some("new".into()),
        aliases: Some(strings(&[
            "WWW.Example.com",
            "www.example.com",
            "cdn.example.com",
        ])),
        default_root_object: Some(String::new()),
        price_class: Some("PriceClass_200".into()),
        enabled: Some(false),
    };
    let result = cloudfront
        .update_distribution("EA", &edit, Some(ETAG))
        .await
        .unwrap();
    assert_eq!(result.etag.as_deref(), Some("E-NEW"));

    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    let (config, if_match) = &sent[0];
    assert_eq!(if_match.as_deref(), Some(ETAG));
    assert_eq!(config.comment(), "new");
    assert!(!config.enabled());
    assert_eq!(config.default_root_object(), Some(""));
    assert_eq!(config.price_class(), Some(&PriceClass::PriceClass200));
    let aliases = config.aliases().unwrap();
    assert_eq!(aliases.quantity(), 2);
    assert_eq!(aliases.items(), ["www.example.com", "cdn.example.com"]);
    // Untouched parts are sent back as they were.
    assert_eq!(config.caller_reference(), "ref-1");
    assert_eq!(config.origins().unwrap().quantity(), 2);
    assert_eq!(
        config.default_cache_behavior(),
        original.default_cache_behavior()
    );
}

#[tokio::test]
async fn an_empty_edit_keeps_the_configuration() {
    let original = config(true, "same", vec![bucket_origin()]);
    let get_config = get_config_rule("EA", original.clone(), ETAG);
    let sent: Sent = Arc::default();
    let update = update_rule(&sent);
    let get = get_rule("EA", original.clone(), ETAG);
    let cloudfront = sequential(&[&get_config, &update, &get]);
    cloudfront
        .update_distribution("EA", &DistributionEdit::default(), None)
        .await
        .unwrap();
    assert_eq!(sent.lock().unwrap()[0].0, original);
}

#[tokio::test]
async fn a_stale_etag_sends_nothing() {
    let get_config = get_config_rule("EA", config(true, "x", vec![bucket_origin()]), "E-CURRENT");
    let sent: Sent = Arc::default();
    let update = update_rule(&sent);
    let cloudfront = client_with(&[&get_config, &update], RuleMode::MatchAny);
    let edit = DistributionEdit {
        comment: Some("mine".into()),
        ..DistributionEdit::default()
    };
    let error = cloudfront
        .update_distribution("EA", &edit, Some("E-OLD"))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Conflict);
    assert!(
        error.to_string().contains("changed since it was loaded"),
        "{error}"
    );
    assert_eq!(update.num_calls(), 0);
}

#[tokio::test]
async fn a_precondition_failure_from_the_service_is_a_conflict() {
    let get_config = get_config_rule("EA", config(true, "x", vec![bucket_origin()]), ETAG);
    let update = mock!(aws_sdk_cloudfront::Client::update_distribution)
        .then_http_response(http_error(412, "PreconditionFailed"));
    let cloudfront = sequential(&[&get_config, &update]);
    let error = cloudfront.set_enabled("EA", false, None).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Conflict);
    assert!(error.to_string().contains("changed elsewhere"), "{error}");
    assert_eq!(update.num_calls(), 1);
}

#[tokio::test]
async fn an_invalid_argument_is_a_usage_error() {
    let get_config = get_config_rule("EA", config(true, "x", vec![bucket_origin()]), ETAG);
    let update = mock!(aws_sdk_cloudfront::Client::update_distribution)
        .then_http_response(http_error(400, "InvalidViewerCertificate"));
    let cloudfront = sequential(&[&get_config, &update]);
    let edit = DistributionEdit {
        aliases: Some(strings(&["www.example.com"])),
        ..DistributionEdit::default()
    };
    let error = cloudfront
        .update_distribution("EA", &edit, None)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Usage);
}

#[tokio::test]
async fn invalid_edits_read_nothing() {
    let get_config = get_config_rule("EA", config(true, "x", vec![bucket_origin()]), ETAG);
    let cloudfront = sequential(&[&get_config]);
    let edits = [
        DistributionEdit {
            comment: Some("x".repeat(MAX_COMMENT_CHARS + 1)),
            ..DistributionEdit::default()
        },
        DistributionEdit {
            aliases: Some(strings(&["not a domain"])),
            ..DistributionEdit::default()
        },
        DistributionEdit {
            default_root_object: Some("/index.html".into()),
            ..DistributionEdit::default()
        },
        DistributionEdit {
            price_class: Some("PriceClass_1".into()),
            ..DistributionEdit::default()
        },
    ];
    for edit in edits {
        let error = cloudfront
            .update_distribution("EA", &edit, None)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Usage, "{edit:?}");
    }
    assert_eq!(get_config.num_calls(), 0);
}

#[tokio::test]
async fn enabling_and_disabling_change_only_the_flag() {
    for enabled in [false, true] {
        let get_config =
            get_config_rule("EA", config(!enabled, "keep", vec![bucket_origin()]), ETAG);
        let sent: Sent = Arc::default();
        let update = update_rule(&sent);
        let get = get_rule("EA", config(enabled, "keep", vec![bucket_origin()]), "E2");
        let cloudfront = sequential(&[&get_config, &update, &get]);
        let result = cloudfront
            .set_enabled("EA", enabled, Some(ETAG))
            .await
            .unwrap();
        assert_eq!(result.enabled, enabled);
        let sent = sent.lock().unwrap();
        assert_eq!(sent[0].0.enabled(), enabled);
        assert_eq!(sent[0].0.comment(), "keep");
        assert_eq!(sent[0].1.as_deref(), Some(ETAG));
    }
}

// ---- detach ----

#[tokio::test]
async fn detaching_removes_only_the_origins_of_the_bucket() {
    let mut original = config(true, "x", vec![other_origin(), bucket_origin()]);
    original.default_cache_behavior = Some(behavior("api"));
    let get_config = get_config_rule("EA", original, ETAG);
    let sent: Sent = Arc::default();
    let update = update_rule(&sent);
    let get = get_rule("EA", config(true, "x", vec![other_origin()]), "E2");
    let cloudfront = sequential(&[&get_config, &update, &get]);
    cloudfront.detach_bucket("EA", "site", None).await.unwrap();
    let sent = sent.lock().unwrap();
    let origins = sent[0].0.origins().unwrap();
    assert_eq!(origins.quantity(), 1);
    assert_eq!(origins.items()[0].id(), "api");
    assert_eq!(sent[0].1.as_deref(), Some(ETAG));
}

#[tokio::test]
async fn the_last_origin_cannot_be_detached() {
    let get_config = get_config_rule("EA", config(true, "x", vec![bucket_origin()]), ETAG);
    let sent: Sent = Arc::default();
    let update = update_rule(&sent);
    let cloudfront = client_with(&[&get_config, &update], RuleMode::MatchAny);
    let error = cloudfront
        .detach_bucket("EA", "site", None)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Conflict);
    assert!(error.to_string().contains("only origin"), "{error}");
    assert_eq!(update.num_calls(), 0);
}

#[test]
fn a_bucket_that_is_not_an_origin_is_not_found() {
    let mut config = config(true, "x", vec![other_origin(), bucket_origin()]);
    config.default_cache_behavior = Some(behavior("api"));
    let error = detach_origins(&mut config, "elsewhere").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::NotFound);
}

#[test]
fn detaching_is_refused_while_behaviours_or_groups_use_the_origin() {
    // The default behaviour.
    let mut config_a = config(true, "x", vec![bucket_origin(), other_origin()]);
    let error = detach_origins(&mut config_a, "site").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Conflict);
    assert!(
        error.to_string().contains("the default behaviour"),
        "{error}"
    );

    // A path behaviour.
    let mut config_b = config(true, "x", vec![other_origin(), bucket_origin()]);
    let extra = CacheBehavior::builder()
        .path_pattern("/static/*")
        .target_origin_id("s3-site")
        .viewer_protocol_policy(ViewerProtocolPolicy::AllowAll)
        .build()
        .unwrap();
    config_b.cache_behaviors = Some(
        CacheBehaviors::builder()
            .quantity(1)
            .items(extra)
            .build()
            .unwrap(),
    );
    let error = detach_origins(&mut config_b, "site").unwrap_err();
    assert!(error.to_string().contains("'/static/*'"), "{error}");

    // An origin group.
    let mut config_c = config(true, "x", vec![other_origin(), bucket_origin()]);
    let member = |id: &str| OriginGroupMember::builder().origin_id(id).build().unwrap();
    let group = OriginGroup::builder()
        .id("failover")
        .members(
            OriginGroupMembers::builder()
                .quantity(2)
                .items(member("api"))
                .items(member("s3-site"))
                .build()
                .unwrap(),
        )
        .failover_criteria(
            aws_sdk_cloudfront::types::OriginGroupFailoverCriteria::builder()
                .status_codes(
                    aws_sdk_cloudfront::types::StatusCodes::builder()
                        .quantity(1)
                        .items(503)
                        .build()
                        .unwrap(),
                )
                .build(),
        )
        .build()
        .unwrap();
    config_c.origin_groups = Some(
        OriginGroups::builder()
            .quantity(1)
            .items(group)
            .build()
            .unwrap(),
    );
    config_c.default_cache_behavior = Some(behavior("api"));
    let error = detach_origins(&mut config_c, "site").unwrap_err();
    assert!(
        error.to_string().contains("origin group 'failover'"),
        "{error}"
    );
}

// ---- invalidations ----

fn sdk_invalidation(id: &str, status: &str, secs: i64, paths: &[&str]) -> SdkInvalidation {
    SdkInvalidation::builder()
        .id(id)
        .status(status)
        .create_time(SmithyDateTime::from_secs(secs))
        .invalidation_batch(
            InvalidationBatch::builder()
                .paths(
                    Paths::builder()
                        .quantity(paths.len() as i32)
                        .set_items(Some(strings(paths)))
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

#[tokio::test]
async fn an_invalidation_sends_the_paths_with_a_fresh_reference() {
    let seen: Arc<Mutex<Vec<InvalidationBatch>>> = Arc::default();
    let record = Arc::clone(&seen);
    let rule = mock!(aws_sdk_cloudfront::Client::create_invalidation)
        .match_requests(move |input| {
            assert_eq!(input.distribution_id(), Some("EA"));
            record
                .lock()
                .unwrap()
                .push(input.invalidation_batch().unwrap().clone());
            true
        })
        .then_output(|| {
            CreateInvalidationOutput::builder()
                .invalidation(sdk_invalidation(
                    "I1",
                    "InProgress",
                    1_760_000_000,
                    &["/*", "/a.html"],
                ))
                .build()
        });
    let cloudfront = client_with(&[&rule], RuleMode::MatchAny);
    let paths = strings(&["/*", " /a.html", "/a.html"]);
    let first = cloudfront.create_invalidation("EA", &paths).await.unwrap();
    cloudfront.create_invalidation("EA", &paths).await.unwrap();
    assert_eq!(first.id, "I1");
    assert_eq!(first.status, "InProgress");
    assert_eq!(first.paths, ["/*", "/a.html"]);
    assert!(first.create_time.is_some());
    let sent = seen.lock().unwrap();
    let items = sent[0].paths().unwrap();
    assert_eq!(items.quantity(), 2);
    assert_eq!(items.items(), ["/*", "/a.html"]);
    assert!(!sent[0].caller_reference().is_empty());
    assert_ne!(sent[0].caller_reference(), sent[1].caller_reference());
}

#[tokio::test]
async fn bad_paths_are_not_sent() {
    let rule = mock!(aws_sdk_cloudfront::Client::create_invalidation)
        .then_output(|| CreateInvalidationOutput::builder().build());
    let cloudfront = client_with(&[&rule], RuleMode::MatchAny);
    for bad in [vec!["/a*b"], vec!["a"], vec![], vec!["/ok", ""]] {
        let error = cloudfront
            .create_invalidation("EA", &strings(&bad))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Usage, "{bad:?}");
    }
    assert_eq!(rule.num_calls(), 0);
}

#[tokio::test]
async fn too_many_running_invalidations_is_a_conflict() {
    let rule = mock!(aws_sdk_cloudfront::Client::create_invalidation)
        .then_http_response(http_error(429, "TooManyInvalidationsInProgress"));
    let cloudfront = client_with(&[&rule], RuleMode::MatchAny);
    let error = cloudfront
        .create_invalidation("EA", &strings(&["/*"]))
        .await
        .unwrap_err();
    // 429 is retried by the SDK, so only the final answer matters.
    assert_eq!(error.kind(), ErrorKind::Conflict);
    assert!(error.to_string().contains("invalidating EA"), "{error}");
}

#[tokio::test]
async fn an_unknown_distribution_cannot_be_invalidated() {
    let rule = mock!(aws_sdk_cloudfront::Client::create_invalidation)
        .then_http_response(http_error(404, "NoSuchDistribution"));
    let cloudfront = sequential(&[&rule]);
    let error = cloudfront
        .create_invalidation("EZ", &strings(&["/*"]))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::NotFound);
}

fn history(
    items: Vec<aws_sdk_cloudfront::types::InvalidationSummary>,
    next: Option<&str>,
) -> ListInvalidationsOutput {
    let mut list = InvalidationList::builder()
        .marker("")
        .max_items(HISTORY_PAGE as i32)
        .is_truncated(next.is_some())
        .quantity(items.len() as i32)
        .set_items(Some(items));
    if let Some(next) = next {
        list = list.next_marker(next);
    }
    ListInvalidationsOutput::builder()
        .invalidation_list(list.build().unwrap())
        .build()
}

fn history_item(
    id: &str,
    status: &str,
    secs: i64,
) -> aws_sdk_cloudfront::types::InvalidationSummary {
    aws_sdk_cloudfront::types::InvalidationSummary::builder()
        .id(id)
        .status(status)
        .create_time(SmithyDateTime::from_secs(secs))
        .build()
        .unwrap()
}

#[tokio::test]
async fn the_history_is_a_timeline_in_the_order_the_service_gives() {
    let first = mock!(aws_sdk_cloudfront::Client::list_invalidations)
        .match_requests(|input| {
            input.distribution_id() == Some("EA")
                && input.marker().is_none()
                && input.max_items() == Some(HISTORY_PAGE as i32)
        })
        .then_output(|| {
            history(
                vec![
                    history_item("I3", "InProgress", 1_760_000_300),
                    history_item("I2", "Completed", 1_760_000_200),
                ],
                Some("I2"),
            )
        });
    let second = mock!(aws_sdk_cloudfront::Client::list_invalidations)
        .match_requests(|input| input.marker() == Some("I2"))
        .then_output(|| history(vec![history_item("I1", "Completed", 1_760_000_100)], None));
    let cloudfront = sequential(&[&first, &second]);
    let page1 = cloudfront.list_invalidations("EA", None).await.unwrap();
    assert_eq!(page1.next_marker.as_deref(), Some("I2"));
    let statuses: Vec<_> = page1
        .items
        .iter()
        .map(|i| (i.id.as_str(), i.status.as_str()))
        .collect();
    assert_eq!(statuses, [("I3", "InProgress"), ("I2", "Completed")]);
    assert!(page1.items[0].create_time > page1.items[1].create_time);
    assert!(page1.items[0].paths.is_empty());
    let page2 = cloudfront
        .list_invalidations("EA", page1.next_marker.as_deref())
        .await
        .unwrap();
    assert_eq!(page2.next_marker, None);
    assert_eq!(page2.items[0].id, "I1");
}

#[tokio::test]
async fn an_invalidation_is_read_with_its_paths() {
    let rule = mock!(aws_sdk_cloudfront::Client::get_invalidation)
        .match_requests(|input| input.distribution_id() == Some("EA") && input.id() == Some("I1"))
        .then_output(|| {
            GetInvalidationOutput::builder()
                .invalidation(sdk_invalidation(
                    "I1",
                    "Completed",
                    1_760_000_000,
                    &["/x/*"],
                ))
                .build()
        });
    let cloudfront = sequential(&[&rule]);
    let invalidation = cloudfront.get_invalidation("EA", "I1").await.unwrap();
    assert_eq!(invalidation.status, "Completed");
    assert_eq!(invalidation.paths, ["/x/*"]);
}

// ---- saved path sets ----

fn store(dir: &tempfile::TempDir) -> ConfigStore {
    ConfigStore::at(dir.path().join("config.toml"))
}

fn set(name: &str, paths: &[&str]) -> PathSet {
    PathSet {
        name: name.into(),
        paths: strings(paths),
    }
}

#[test]
fn path_sets_are_stored_in_order_and_validated() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    assert!(list_path_sets(&store).unwrap().is_empty());
    save_path_set(&store, set("Site", &["/*"]), None).unwrap();
    let stored = save_path_set(
        &store,
        set(" Images ", &["/img/*", "/img/*", "/logo.png"]),
        None,
    )
    .unwrap();
    assert_eq!(stored[1], set("Images", &["/img/*", "/logo.png"]));
    assert_eq!(
        list_path_sets(&ConfigStore::at(store.path())).unwrap(),
        stored
    );

    assert_eq!(
        save_path_set(&store, set("site", &["/x"]), None)
            .unwrap_err()
            .kind(),
        ErrorKind::Conflict
    );
    assert_eq!(
        save_path_set(&store, set("", &["/x"]), None)
            .unwrap_err()
            .kind(),
        ErrorKind::Usage
    );
    assert_eq!(
        save_path_set(&store, set("Bad", &["/a*b"]), None)
            .unwrap_err()
            .kind(),
        ErrorKind::Usage
    );
    assert_eq!(
        save_path_set(&store, set("Bad\nname", &["/a"]), None)
            .unwrap_err()
            .kind(),
        ErrorKind::Usage
    );
    assert_eq!(list_path_sets(&store).unwrap(), stored);
}

#[test]
fn replacing_a_set_keeps_its_place() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    for name in ["a", "b", "c"] {
        save_path_set(&store, set(name, &["/x"]), None).unwrap();
    }
    let stored = save_path_set(&store, set("B2", &["/y/*"]), Some("B")).unwrap();
    let names: Vec<_> = stored.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["a", "B2", "c"]);
    assert_eq!(
        save_path_set(&store, set("A", &["/z"]), Some("B2"))
            .unwrap_err()
            .kind(),
        ErrorKind::Conflict
    );
    assert_eq!(
        save_path_set(&store, set("n", &["/z"]), Some("missing"))
            .unwrap_err()
            .kind(),
        ErrorKind::NotFound
    );
}

#[test]
fn deleting_a_set_ignores_case() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    save_path_set(&store, set("Site", &["/*"]), None).unwrap();
    assert!(delete_path_set(&store, "SITE").unwrap().is_empty());
    assert_eq!(
        delete_path_set(&store, "Site").unwrap_err().kind(),
        ErrorKind::NotFound
    );
}

#[test]
fn the_number_of_sets_is_limited() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    for i in 0..MAX_PATH_SETS {
        save_path_set(&store, set(&format!("set {i}"), &["/x"]), None).unwrap();
    }
    assert_eq!(
        save_path_set(&store, set("one more", &["/x"]), None)
            .unwrap_err()
            .kind(),
        ErrorKind::Conflict
    );
}
