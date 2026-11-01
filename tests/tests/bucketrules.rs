//! Bucket rules (`bucketbridge_core::bucketrules`): lifecycle,
//! Intelligent-Tiering, CORS and replication.
//!
//! The round trips run against `RulesMock`, a small stateful emulation on top
//! of the recorded [`MockS3`] (s3s-fs implements none of these APIs); every
//! test asserts the exact XML that was sent. The tests that need a real
//! server (lifecycle and CORS) are ignored unless the `minio` feature is
//! enabled (see docs/testing.md).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use bucketbridge_core::ErrorKind;
use bucketbridge_core::bucketcfg::Section;
use bucketbridge_core::bucketrules::{
    AccessTier, CorsConfig, CorsRule, ExpirationAction, LifecycleConfig, LifecycleFilter,
    LifecycleRule, LifecycleTransition, NoncurrentExpiration, NoncurrentTransition, ObjectFilter,
    ReplicationConfig, ReplicationRule, RuleStatus, Tag, TieringConfig, TieringStep,
};
use bucketbridge_tests::{RulesMock, TestS3};

const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";

/// The body of the last request, which must be `method` on `subresource` of
/// the bucket `site`.
fn last_body(server: &RulesMock, method: &str, subresource: &str) -> String {
    let requests = server.requests();
    let request = requests.last().unwrap();
    assert_eq!(request.method, method, "{request:?}");
    assert_eq!(request.path, "/site/", "{request:?}");
    assert!(
        request.query_param(subresource).is_some(),
        "{subresource}: {request:?}"
    );
    request.body_text()
}

fn tag(key: &str, value: &str) -> Tag {
    Tag {
        key: key.into(),
        value: value.into(),
    }
}

fn lifecycle() -> LifecycleConfig {
    LifecycleConfig {
        rules: vec![
            LifecycleRule {
                id: Some("archive-logs".into()),
                filter: LifecycleFilter {
                    prefix: Some("logs/".into()),
                    tags: vec![tag("kind", "log")],
                    object_size_greater_than: Some(1024),
                    object_size_less_than: Some(1_048_576),
                },
                transitions: vec![
                    LifecycleTransition {
                        days: Some(30),
                        storage_class: "STANDARD_IA".into(),
                        ..LifecycleTransition::default()
                    },
                    LifecycleTransition {
                        date: Some("2031-01-31".into()),
                        storage_class: "GLACIER".into(),
                        ..LifecycleTransition::default()
                    },
                ],
                expiration: Some(ExpirationAction {
                    days: Some(3650),
                    ..ExpirationAction::default()
                }),
                noncurrent_version_transitions: vec![NoncurrentTransition {
                    noncurrent_days: 30,
                    newer_noncurrent_versions: Some(2),
                    storage_class: "GLACIER_IR".into(),
                }],
                noncurrent_version_expiration: Some(NoncurrentExpiration {
                    noncurrent_days: Some(90),
                    newer_noncurrent_versions: Some(5),
                }),
                ..LifecycleRule::default()
            },
            LifecycleRule {
                id: Some("tidy".into()),
                status: RuleStatus::Disabled,
                expiration: Some(ExpirationAction {
                    expired_object_delete_marker: true,
                    ..ExpirationAction::default()
                }),
                abort_incomplete_multipart_upload_days: Some(7),
                ..LifecycleRule::default()
            },
            LifecycleRule {
                id: Some("big".into()),
                filter: LifecycleFilter {
                    object_size_greater_than: Some(5_000_000),
                    ..LifecycleFilter::default()
                },
                expiration: Some(ExpirationAction {
                    date: Some("2032-06-01".into()),
                    ..ExpirationAction::default()
                }),
                ..LifecycleRule::default()
            },
        ],
    }
}

// ---- lifecycle ----

#[tokio::test]
async fn lifecycle_round_trips_with_exact_xml() {
    let server = RulesMock::start();
    let s3 = server.ops();
    assert_eq!(s3.get_bucket_lifecycle("site").await.unwrap(), None);

    let config = lifecycle();
    s3.put_bucket_lifecycle("site", &config).await.unwrap();
    assert_eq!(
        last_body(&server, "PUT", "lifecycle"),
        format!(
            "<LifecycleConfiguration {NS}>\
<Rule><Expiration><Days>3650</Days></Expiration><ID>archive-logs</ID>\
<Filter><And><Prefix>logs/</Prefix><Tag><Key>kind</Key><Value>log</Value></Tag><ObjectSizeGreaterThan>1024</ObjectSizeGreaterThan><ObjectSizeLessThan>1048576</ObjectSizeLessThan></And></Filter>\
<Status>Enabled</Status>\
<Transition><Days>30</Days><StorageClass>STANDARD_IA</StorageClass></Transition>\
<Transition><Date>2031-01-31T00:00:00Z</Date><StorageClass>GLACIER</StorageClass></Transition>\
<NoncurrentVersionTransition><NoncurrentDays>30</NoncurrentDays><StorageClass>GLACIER_IR</StorageClass><NewerNoncurrentVersions>2</NewerNoncurrentVersions></NoncurrentVersionTransition>\
<NoncurrentVersionExpiration><NoncurrentDays>90</NoncurrentDays><NewerNoncurrentVersions>5</NewerNoncurrentVersions></NoncurrentVersionExpiration></Rule>\
<Rule><Expiration><ExpiredObjectDeleteMarker>true</ExpiredObjectDeleteMarker></Expiration><ID>tidy</ID><Filter></Filter><Status>Disabled</Status>\
<AbortIncompleteMultipartUpload><DaysAfterInitiation>7</DaysAfterInitiation></AbortIncompleteMultipartUpload></Rule>\
<Rule><Expiration><Date>2032-06-01T00:00:00Z</Date></Expiration><ID>big</ID><Filter><ObjectSizeGreaterThan>5000000</ObjectSizeGreaterThan></Filter><Status>Enabled</Status></Rule>\
</LifecycleConfiguration>"
        )
    );

    // Read back: the same model.
    assert_eq!(
        s3.get_bucket_lifecycle("site").await.unwrap(),
        Some(config.clone())
    );

    // Edit what was read, write, read again.
    let mut edited = config;
    edited.rules[1].status = RuleStatus::Enabled;
    edited.rules.truncate(2);
    s3.put_bucket_lifecycle("site", &edited).await.unwrap();
    let again = s3.get_bucket_lifecycle("site").await.unwrap().unwrap();
    assert_eq!(again, edited);
    assert_eq!(again.rules.len(), 2);

    s3.delete_bucket_lifecycle("site").await.unwrap();
    let requests = server.requests();
    let delete = requests.last().unwrap();
    assert_eq!(delete.method, "DELETE");
    assert!(delete.query_param("lifecycle").is_some());
    assert_eq!(s3.get_bucket_lifecycle("site").await.unwrap(), None);
    // Deleting nothing succeeds.
    s3.delete_bucket_lifecycle("site").await.unwrap();
}

#[tokio::test]
async fn single_filter_conditions_are_sent_without_and() {
    let server = RulesMock::start();
    let s3 = server.ops();
    let rule = |filter: LifecycleFilter| LifecycleRule {
        id: Some("r".into()),
        filter,
        expiration: Some(ExpirationAction {
            days: Some(1),
            ..ExpirationAction::default()
        }),
        ..LifecycleRule::default()
    };
    for (filter, expected) in [
        (
            LifecycleFilter {
                prefix: Some("a/".into()),
                ..LifecycleFilter::default()
            },
            "<Filter><Prefix>a/</Prefix></Filter>",
        ),
        (
            LifecycleFilter {
                tags: vec![tag("k", "v")],
                ..LifecycleFilter::default()
            },
            "<Filter><Tag><Key>k</Key><Value>v</Value></Tag></Filter>",
        ),
        (
            LifecycleFilter {
                object_size_less_than: Some(9),
                ..LifecycleFilter::default()
            },
            "<Filter><ObjectSizeLessThan>9</ObjectSizeLessThan></Filter>",
        ),
        (
            LifecycleFilter {
                tags: vec![tag("a", "1"), tag("b", "2")],
                ..LifecycleFilter::default()
            },
            "<Filter><And><Tag><Key>a</Key><Value>1</Value></Tag><Tag><Key>b</Key><Value>2</Value></Tag></And></Filter>",
        ),
    ] {
        let config = LifecycleConfig {
            rules: vec![rule(filter.clone())],
        };
        s3.put_bucket_lifecycle("site", &config).await.unwrap();
        assert!(
            last_body(&server, "PUT", "lifecycle").contains(expected),
            "{expected}"
        );
        assert_eq!(s3.get_bucket_lifecycle("site").await.unwrap(), Some(config));
    }
}

#[tokio::test]
async fn an_old_style_rule_prefix_is_read_as_the_filter() {
    let server = RulesMock::start();
    server.store(
        "site",
        "lifecycle",
        &format!(
            "<LifecycleConfiguration {NS}><Rule><ID>old</ID><Prefix>tmp/</Prefix><Status>Enabled</Status><Expiration><Days>1</Days></Expiration></Rule></LifecycleConfiguration>"
        ),
    );
    let read = server
        .ops()
        .get_bucket_lifecycle("site")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read.rules[0].filter.prefix.as_deref(), Some("tmp/"));
    assert_eq!(read.rules[0].expiration.as_ref().unwrap().days, Some(1));
}

#[tokio::test]
async fn an_invalid_lifecycle_is_never_sent() {
    let server = RulesMock::start();
    let s3 = server.ops();
    let mut config = lifecycle();
    config.rules[0].transitions[0].days = Some(10);
    config.rules[1].abort_incomplete_multipart_upload_days = Some(0);
    let error = s3.put_bucket_lifecycle("site", &config).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Usage);
    let text = error.to_string();
    assert!(
        text.contains("rules[0].transitions[0].days: objects stay at least 30 days"),
        "{text}"
    );
    assert!(
        text.contains("rules[1].abort_incomplete_multipart_upload_days: must be at least 1"),
        "{text}"
    );
    assert!(server.requests().is_empty(), "nothing may be sent");
}

// ---- Intelligent-Tiering ----

fn tiering(id: &str, prefix: &str) -> TieringConfig {
    TieringConfig {
        id: id.into(),
        status: RuleStatus::Enabled,
        filter: ObjectFilter {
            prefix: Some(prefix.into()),
            tags: vec![],
        },
        tierings: vec![
            TieringStep {
                access_tier: AccessTier::ArchiveAccess,
                days: 90,
            },
            TieringStep {
                access_tier: AccessTier::DeepArchiveAccess,
                days: 180,
            },
        ],
    }
}

#[tokio::test]
async fn tiering_configurations_are_listed_read_written_and_deleted() {
    let server = RulesMock::start();
    let s3 = server.ops();
    assert_eq!(s3.list_bucket_tiering("site").await.unwrap(), vec![]);
    assert_eq!(s3.get_bucket_tiering("site", "nope").await.unwrap(), None);

    let beta = tiering("beta", "b/");
    s3.put_bucket_tiering("site", &beta).await.unwrap();
    let requests = server.requests();
    let put = requests.last().unwrap();
    assert_eq!(put.method, "PUT");
    assert_eq!(put.query_param("id").as_deref(), Some("beta"));
    assert_eq!(
        put.body_text(),
        format!(
            "<IntelligentTieringConfiguration {NS}><Id>beta</Id><Filter><Prefix>b/</Prefix></Filter><Status>Enabled</Status>\
<Tiering><Days>90</Days><AccessTier>ARCHIVE_ACCESS</AccessTier></Tiering>\
<Tiering><Days>180</Days><AccessTier>DEEP_ARCHIVE_ACCESS</AccessTier></Tiering></IntelligentTieringConfiguration>"
        )
    );

    let mut alpha = tiering("alpha", "");
    alpha.filter = ObjectFilter {
        prefix: Some("a/".into()),
        tags: vec![tag("t", "1")],
    };
    alpha.status = RuleStatus::Disabled;
    alpha.tierings.truncate(1);
    s3.put_bucket_tiering("site", &alpha).await.unwrap();
    assert!(
        server
            .stored("site", "intelligent-tiering/alpha")
            .unwrap()
            .contains("<Filter><And><Prefix>a/</Prefix><Tag><Key>t</Key><Value>1</Value></Tag></And></Filter><Status>Disabled</Status>")
    );
    let mut bare = tiering("bare", "");
    bare.filter = ObjectFilter::default();
    s3.put_bucket_tiering("site", &bare).await.unwrap();
    assert!(
        !server
            .stored("site", "intelligent-tiering/bare")
            .unwrap()
            .contains("<Filter>")
    );

    assert_eq!(
        s3.list_bucket_tiering("site").await.unwrap(),
        vec![alpha.clone(), bare.clone(), beta.clone()]
    );
    assert_eq!(
        s3.get_bucket_tiering("site", "beta").await.unwrap(),
        Some(beta)
    );

    s3.delete_bucket_tiering("site", "alpha").await.unwrap();
    let requests = server.requests();
    let delete = requests.last().unwrap();
    assert_eq!(delete.method, "DELETE");
    assert_eq!(delete.query_param("id").as_deref(), Some("alpha"));
    assert_eq!(s3.list_bucket_tiering("site").await.unwrap().len(), 2);
    s3.delete_bucket_tiering("site", "alpha").await.unwrap();

    let mut invalid = bare;
    invalid.tierings[0].days = 10;
    let before = server.requests().len();
    let error = s3.put_bucket_tiering("site", &invalid).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Usage);
    assert!(
        error
            .to_string()
            .contains("tierings[0].days: ARCHIVE_ACCESS takes 90 to 730 days (found 10)")
    );
    assert_eq!(server.requests().len(), before);
}

// ---- CORS ----

#[tokio::test]
async fn cors_round_trips_with_exact_xml() {
    let server = RulesMock::start();
    let s3 = server.ops();
    assert_eq!(s3.get_bucket_cors("site").await.unwrap(), None);
    let config = CorsConfig {
        rules: vec![
            CorsRule {
                id: Some("web".into()),
                allowed_origins: vec![
                    "https://app.example.org".into(),
                    "https://*.example.net".into(),
                ],
                allowed_methods: vec!["GET".into(), "PUT".into()],
                allowed_headers: vec!["*".into()],
                expose_headers: vec!["ETag".into(), "x-amz-request-id".into()],
                max_age_seconds: Some(3000),
            },
            CorsRule {
                allowed_origins: vec!["*".into()],
                allowed_methods: vec!["GET".into()],
                ..CorsRule::default()
            },
        ],
    };
    s3.put_bucket_cors("site", &config).await.unwrap();
    assert_eq!(
        last_body(&server, "PUT", "cors"),
        format!(
            "<CORSConfiguration {NS}>\
<CORSRule><ID>web</ID><AllowedHeader>*</AllowedHeader><AllowedMethod>GET</AllowedMethod><AllowedMethod>PUT</AllowedMethod>\
<AllowedOrigin>https://app.example.org</AllowedOrigin><AllowedOrigin>https://*.example.net</AllowedOrigin>\
<ExposeHeader>ETag</ExposeHeader><ExposeHeader>x-amz-request-id</ExposeHeader><MaxAgeSeconds>3000</MaxAgeSeconds></CORSRule>\
<CORSRule><AllowedMethod>GET</AllowedMethod><AllowedOrigin>*</AllowedOrigin></CORSRule></CORSConfiguration>"
        )
    );
    assert_eq!(
        s3.get_bucket_cors("site").await.unwrap(),
        Some(config.clone())
    );

    let mut edited = config;
    edited.rules[1].allowed_methods.push("HEAD".into());
    s3.put_bucket_cors("site", &edited).await.unwrap();
    assert_eq!(s3.get_bucket_cors("site").await.unwrap(), Some(edited));

    s3.delete_bucket_cors("site").await.unwrap();
    assert_eq!(s3.get_bucket_cors("site").await.unwrap(), None);
    s3.delete_bucket_cors("site").await.unwrap();

    let before = server.requests().len();
    let error = s3
        .put_bucket_cors("site", &CorsConfig::default())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Usage);
    assert_eq!(server.requests().len(), before);
}

// ---- replication ----

fn replication() -> ReplicationConfig {
    ReplicationConfig {
        role: "arn:aws:iam::123456789012:role/replicate".into(),
        rules: vec![
            ReplicationRule {
                id: Some("everything".into()),
                priority: 2,
                status: RuleStatus::Enabled,
                filter: ObjectFilter::default(),
                destination_bucket: "arn:aws:s3:::backup".into(),
                storage_class: Some("STANDARD_IA".into()),
                delete_marker_replication: true,
            },
            ReplicationRule {
                id: Some("docs".into()),
                priority: 1,
                status: RuleStatus::Disabled,
                filter: ObjectFilter {
                    prefix: Some("docs/".into()),
                    tags: vec![tag("keep", "yes")],
                },
                destination_bucket: "arn:aws:s3:::backup-docs".into(),
                storage_class: None,
                delete_marker_replication: false,
            },
        ],
    }
}

#[tokio::test]
async fn replication_needs_versioning_on_the_source() {
    let server = RulesMock::start();
    let s3 = server.ops();
    for (state, text) in [("", "not enabled"), ("Suspended", "suspended")] {
        server.set_versioning(state);
        let error = s3
            .put_bucket_replication("site", &replication())
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Usage);
        assert_eq!(
            error.to_string(),
            format!(
                "replication needs versioning on the source bucket, and versioning of 'site' is {text}; enable versioning in the bucket properties first"
            )
        );
    }
    // Only the versioning reads were sent, never a PUT.
    assert!(server.requests().iter().all(|r| r.method == "GET"));
    assert!(server.stored("site", "replication").is_none());
}

#[tokio::test]
async fn replication_round_trips_with_exact_xml() {
    let server = RulesMock::start();
    server.set_versioning("Enabled");
    let s3 = server.ops();
    assert_eq!(s3.get_bucket_replication("site").await.unwrap(), None);

    let config = replication();
    s3.put_bucket_replication("site", &config).await.unwrap();
    assert_eq!(
        last_body(&server, "PUT", "replication"),
        format!(
            "<ReplicationConfiguration {NS}><Role>arn:aws:iam::123456789012:role/replicate</Role>\
<Rule><ID>everything</ID><Priority>2</Priority><Filter><Prefix></Prefix></Filter><Status>Enabled</Status>\
<Destination><Bucket>arn:aws:s3:::backup</Bucket><StorageClass>STANDARD_IA</StorageClass></Destination>\
<DeleteMarkerReplication><Status>Enabled</Status></DeleteMarkerReplication></Rule>\
<Rule><ID>docs</ID><Priority>1</Priority><Filter><And><Prefix>docs/</Prefix><Tag><Key>keep</Key><Value>yes</Value></Tag></And></Filter><Status>Disabled</Status>\
<Destination><Bucket>arn:aws:s3:::backup-docs</Bucket></Destination>\
<DeleteMarkerReplication><Status>Disabled</Status></DeleteMarkerReplication></Rule></ReplicationConfiguration>"
        )
    );
    assert_eq!(
        s3.get_bucket_replication("site").await.unwrap(),
        Some(config.clone())
    );

    s3.delete_bucket_replication("site").await.unwrap();
    assert_eq!(s3.get_bucket_replication("site").await.unwrap(), None);
    s3.delete_bucket_replication("site").await.unwrap();

    let mut bad = config;
    bad.rules[0].destination_bucket = "backup".into();
    let before = server.requests().len();
    let error = s3.put_bucket_replication("site", &bad).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Usage);
    assert!(error.to_string().contains("rules[0].destination_bucket"));
    assert_eq!(
        server.requests().len(),
        before,
        "invalid input is checked before any read"
    );
}

#[tokio::test]
async fn replication_keeps_the_settings_the_model_does_not_carry() {
    let server = RulesMock::start();
    server.set_versioning("Enabled");
    server.store(
        "site",
        "replication",
        &format!(
            "<ReplicationConfiguration {NS}><Role>arn:aws:iam::123456789012:role/replicate</Role>\
<Rule><ID>everything</ID><Priority>2</Priority><Filter><Prefix></Prefix></Filter><Status>Enabled</Status>\
<SourceSelectionCriteria><ReplicaModifications><Status>Enabled</Status></ReplicaModifications></SourceSelectionCriteria>\
<ExistingObjectReplication><Status>Enabled</Status></ExistingObjectReplication>\
<Destination><Bucket>arn:aws:s3:::backup</Bucket><Account>210987654321</Account>\
<ReplicationTime><Status>Enabled</Status><Time><Minutes>15</Minutes></Time></ReplicationTime>\
<Metrics><Status>Enabled</Status><EventThreshold><Minutes>15</Minutes></EventThreshold></Metrics></Destination>\
<DeleteMarkerReplication><Status>Disabled</Status></DeleteMarkerReplication></Rule></ReplicationConfiguration>"
        ),
    );
    let s3 = server.ops();
    let mut config = s3.get_bucket_replication("site").await.unwrap().unwrap();
    assert_eq!(config.rules[0].destination_bucket, "arn:aws:s3:::backup");
    config.rules[0].delete_marker_replication = true;
    s3.put_bucket_replication("site", &config).await.unwrap();
    let body = last_body(&server, "PUT", "replication");
    for kept in [
        "<ReplicaModifications><Status>Enabled</Status></ReplicaModifications>",
        "<ExistingObjectReplication><Status>Enabled</Status></ExistingObjectReplication>",
        "<Account>210987654321</Account>",
        "<ReplicationTime><Status>Enabled</Status><Time><Minutes>15</Minutes></Time></ReplicationTime>",
        "<Metrics><Status>Enabled</Status><EventThreshold><Minutes>15</Minutes></EventThreshold></Metrics>",
        "<DeleteMarkerReplication><Status>Enabled</Status></DeleteMarkerReplication>",
    ] {
        assert!(body.contains(kept), "{kept}\n{body}");
    }
}

// ---- sections ----

#[tokio::test]
async fn each_rule_section_is_reported_on_its_own() {
    let server = RulesMock::start();
    server.not_implemented("intelligent-tiering");
    server.deny("replication");
    let s3 = server.ops();
    s3.put_bucket_cors(
        "site",
        &CorsConfig {
            rules: vec![CorsRule {
                allowed_origins: vec!["*".into()],
                allowed_methods: vec!["GET".into()],
                ..CorsRule::default()
            }],
        },
    )
    .await
    .unwrap();
    let rules = s3.get_bucket_rules("site").await.unwrap();
    assert!(matches!(rules.cors, Section::Loaded { value: Some(_) }));
    assert!(matches!(rules.lifecycle, Section::Loaded { value: None }));
    match &rules.tiering {
        Section::Unsupported { message } => {
            assert!(message.starts_with("This server does not support Intelligent-Tiering"));
        }
        other => panic!("{other:?}"),
    }
    match &rules.replication {
        Section::Failed { error } => assert_eq!(error.code, ErrorKind::Auth),
        other => panic!("{other:?}"),
    }
}

// ---- MinIO ----

#[tokio::test]
#[cfg_attr(
    not(feature = "minio"),
    ignore = "needs MinIO: s3s-fs does not implement lifecycle configurations"
)]
async fn lifecycle_round_trips_against_minio() {
    let s3 = TestS3::start_minio().await;
    let bucket = s3.new_bucket().await;
    assert_eq!(s3.ops().get_bucket_lifecycle(&bucket).await.unwrap(), None);
    let config = LifecycleConfig {
        rules: vec![LifecycleRule {
            id: Some("expire-tmp".into()),
            filter: LifecycleFilter {
                prefix: Some("tmp/".into()),
                ..LifecycleFilter::default()
            },
            expiration: Some(ExpirationAction {
                days: Some(7),
                ..ExpirationAction::default()
            }),
            abort_incomplete_multipart_upload_days: Some(2),
            ..LifecycleRule::default()
        }],
    };
    s3.ops()
        .put_bucket_lifecycle(&bucket, &config)
        .await
        .unwrap();
    let read = s3
        .ops()
        .get_bucket_lifecycle(&bucket)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read.rules[0].id.as_deref(), Some("expire-tmp"));
    assert_eq!(read.rules[0].filter.prefix.as_deref(), Some("tmp/"));
    assert_eq!(read.rules[0].expiration.as_ref().unwrap().days, Some(7));
    s3.ops().delete_bucket_lifecycle(&bucket).await.unwrap();
    assert_eq!(s3.ops().get_bucket_lifecycle(&bucket).await.unwrap(), None);
}

#[tokio::test]
#[cfg_attr(
    not(feature = "minio"),
    ignore = "needs MinIO: s3s-fs does not implement CORS configurations"
)]
async fn cors_round_trips_against_minio() {
    let s3 = TestS3::start_minio().await;
    let bucket = s3.new_bucket().await;
    assert_eq!(s3.ops().get_bucket_cors(&bucket).await.unwrap(), None);
    let config = CorsConfig {
        rules: vec![CorsRule {
            allowed_origins: vec!["https://example.org".into()],
            allowed_methods: vec!["GET".into()],
            ..CorsRule::default()
        }],
    };
    match s3.ops().put_bucket_cors(&bucket, &config).await {
        Ok(()) => {
            let read = s3.ops().get_bucket_cors(&bucket).await.unwrap().unwrap();
            assert_eq!(read.rules[0].allowed_origins, ["https://example.org"]);
            s3.ops().delete_bucket_cors(&bucket).await.unwrap();
        }
        // MinIO answers NotImplemented for bucket CORS in some releases.
        Err(error) => assert!(
            bucketbridge_core::bucketcfg::is_unsupported(&error),
            "{error}"
        ),
    }
}
