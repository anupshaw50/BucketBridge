//! Security scan (`bucketbridge_core::scan`): every finding and its absence
//! against a scripted S3 endpoint, partial failures, and the requests sent.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use bucketbridge_core::ErrorKind;
use bucketbridge_core::scan::{Check, FindingId, FixTab, ScanReport, Severity, scan_buckets};
use bucketbridge_tests::{MockResponse, MockS3};

const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";
const ALL_USERS: &str = "http://acs.amazonaws.com/groups/global/AllUsers";
const AUTHENTICATED: &str = "http://acs.amazonaws.com/groups/global/AuthenticatedUsers";

/// How the script answers one subresource.
#[derive(Clone)]
enum Answer {
    /// The normal answer for the bucket's settings.
    Normal,
    /// `403 AccessDenied`.
    Denied,
    /// `501 NotImplemented`.
    Unsupported,
}

#[derive(Clone)]
struct Bucket {
    /// `(group URI, permission)` grants.
    grants: Vec<(&'static str, &'static str)>,
    /// `IsPublic` of `GetBucketPolicyStatus`; `None` is no policy at all.
    policy_public: Option<bool>,
    /// The policy text, for servers without a policy status.
    policy: Option<&'static str>,
    /// The four toggles, or `None` for no public access block.
    block: Option<[bool; 4]>,
    encrypted: bool,
    /// `Enabled`, `Suspended` or empty.
    versioning: &'static str,
    /// `BucketOwnerEnforced`, `ObjectWriter`, or `None` for no controls.
    ownership: Option<&'static str>,
    acl: Answer,
    policy_status: Answer,
    block_answer: Answer,
}

impl Bucket {
    fn clean() -> Bucket {
        Bucket {
            grants: vec![],
            policy_public: None,
            policy: None,
            block: Some([true; 4]),
            encrypted: true,
            versioning: "Enabled",
            ownership: Some("BucketOwnerEnforced"),
            acl: Answer::Normal,
            policy_status: Answer::Normal,
            block_answer: Answer::Normal,
        }
    }

    fn bad() -> Bucket {
        Bucket {
            grants: vec![(ALL_USERS, "WRITE")],
            policy_public: Some(true),
            policy: None,
            block: None,
            encrypted: false,
            versioning: "",
            ownership: Some("ObjectWriter"),
            ..Bucket::clean()
        }
    }
}

fn xml(root: &str, inner: &str) -> MockResponse {
    MockResponse::xml(format!("<{root} {NS}>{inner}</{root}>"))
}

fn denied() -> MockResponse {
    MockResponse::error(403, "AccessDenied", "Access Denied")
}

fn unsupported() -> MockResponse {
    MockResponse::error(501, "NotImplemented", "Not implemented")
}

fn answer(config: &Bucket, query: &str) -> MockResponse {
    let has = |name: &str| {
        query
            .split('&')
            .any(|pair| pair.split('=').next() == Some(name))
    };
    let pick = |kind: &Answer, normal: &dyn Fn() -> MockResponse| match kind {
        Answer::Normal => normal(),
        Answer::Denied => denied(),
        Answer::Unsupported => unsupported(),
    };
    if has("acl") {
        return pick(&config.acl, &|| {
            let grants: String = config
                .grants
                .iter()
                .map(|(uri, permission)| {
                    format!(
                        "<Grant><Grantee xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
                         xsi:type=\"Group\"><URI>{uri}</URI></Grantee>\
                         <Permission>{permission}</Permission></Grant>"
                    )
                })
                .collect();
            xml(
                "AccessControlPolicy",
                &format!(
                    "<Owner><ID>owner</ID></Owner><AccessControlList>{grants}</AccessControlList>"
                ),
            )
        });
    }
    if has("policyStatus") {
        return pick(&config.policy_status, &|| match config.policy_public {
            Some(public) => xml("PolicyStatus", &format!("<IsPublic>{public}</IsPublic>")),
            None => MockResponse::error(404, "NoSuchBucketPolicy", "no policy"),
        });
    }
    if has("policy") {
        return match config.policy {
            Some(text) => MockResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: text.to_owned(),
            },
            None => MockResponse::error(404, "NoSuchBucketPolicy", "no policy"),
        };
    }
    if has("publicAccessBlock") {
        return pick(&config.block_answer, &|| match config.block {
            Some([a, b, c, d]) => xml(
                "PublicAccessBlockConfiguration",
                &format!(
                    "<BlockPublicAcls>{a}</BlockPublicAcls><IgnorePublicAcls>{b}</IgnorePublicAcls>\
                     <BlockPublicPolicy>{c}</BlockPublicPolicy>\
                     <RestrictPublicBuckets>{d}</RestrictPublicBuckets>"
                ),
            ),
            None => MockResponse::error(404, "NoSuchPublicAccessBlockConfiguration", "none"),
        });
    }
    if has("encryption") {
        return if config.encrypted {
            xml(
                "ServerSideEncryptionConfiguration",
                "<Rule><ApplyServerSideEncryptionByDefault><SSEAlgorithm>AES256</SSEAlgorithm>\
                 </ApplyServerSideEncryptionByDefault></Rule>",
            )
        } else {
            MockResponse::error(
                404,
                "ServerSideEncryptionConfigurationNotFoundError",
                "The server side encryption configuration was not found",
            )
        };
    }
    if has("versioning") {
        let status = if config.versioning.is_empty() {
            String::new()
        } else {
            format!("<Status>{}</Status>", config.versioning)
        };
        return xml("VersioningConfiguration", &status);
    }
    if has("ownershipControls") {
        return match config.ownership {
            Some(value) => xml(
                "OwnershipControls",
                &format!("<Rule><ObjectOwnership>{value}</ObjectOwnership></Rule>"),
            ),
            None => MockResponse::error(
                404,
                "OwnershipControlsNotFoundError",
                "The bucket ownership controls were not found",
            ),
        };
    }
    MockResponse::error(400, "UnexpectedRequest", query)
}

fn server(buckets: &[(&str, Bucket)]) -> MockS3 {
    let map: BTreeMap<String, Bucket> = buckets
        .iter()
        .map(|(name, config)| ((*name).to_owned(), config.clone()))
        .collect();
    MockS3::start(move |request| {
        let name = request
            .path
            .trim_matches('/')
            .split('/')
            .next()
            .unwrap_or("");
        match map.get(name) {
            Some(config) => answer(config, &request.query),
            None => MockResponse::error(404, "NoSuchBucket", "no such bucket"),
        }
    })
}

async fn scan(mock: &MockS3, names: &[&str]) -> ScanReport {
    let names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
    scan_buckets(&mock.ops(), &names).await
}

fn ids(report: &ScanReport, bucket: usize) -> Vec<FindingId> {
    report.buckets[bucket]
        .findings
        .iter()
        .map(|f| f.id)
        .collect()
}

#[tokio::test]
async fn a_well_configured_bucket_has_no_findings() {
    let mock = server(&[("tidy", Bucket::clean())]);
    let report = scan(&mock, &["tidy"]).await;
    let scan = &report.buckets[0];
    assert!(scan.findings.is_empty(), "{:?}", scan.findings);
    assert!(scan.skipped.is_empty());
    assert!(scan.error.is_none());
    assert_eq!(report.worst(), None);
}

#[tokio::test]
async fn every_finding_is_reported_with_severity_and_tab() {
    let mock = server(&[("messy", Bucket::bad())]);
    let report = scan(&mock, &["messy"]).await;
    let found = &report.buckets[0].findings;
    let get = |id| {
        found
            .iter()
            .find(|f| f.id == id)
            .unwrap_or_else(|| panic!("{id:?}"))
    };
    assert_eq!(get(FindingId::PublicAcl).severity, Severity::Critical);
    assert_eq!(get(FindingId::PublicAcl).fix_tab, FixTab::PublicAccess);
    assert_eq!(get(FindingId::PublicPolicy).severity, Severity::Critical);
    assert_eq!(get(FindingId::PublicPolicy).fix_tab, FixTab::Policy);
    assert_eq!(
        get(FindingId::PublicAccessBlockOff).severity,
        Severity::High
    );
    assert_eq!(
        get(FindingId::NoDefaultEncryption).fix_tab,
        FixTab::Encryption
    );
    assert_eq!(get(FindingId::VersioningOff).fix_tab, FixTab::Versioning);
    assert_eq!(get(FindingId::AclsEnabled).fix_tab, FixTab::Ownership);
    assert_eq!(found.len(), 6);
    assert!(
        found
            .iter()
            .all(|f| !f.explanation.is_empty() && !f.title.is_empty())
    );
    assert!(found.windows(2).all(|w| w[0].severity >= w[1].severity));
    assert_eq!(report.count(Severity::Critical), 2);
    assert_eq!(report.rows().len(), 6);
}

#[tokio::test]
async fn acl_grants_to_groups_set_the_severity() {
    let mock = server(&[
        (
            "readable",
            Bucket {
                grants: vec![(ALL_USERS, "READ")],
                ..Bucket::clean()
            },
        ),
        (
            "signedin",
            Bucket {
                grants: vec![(AUTHENTICATED, "READ")],
                ..Bucket::clean()
            },
        ),
        (
            "signedinwrite",
            Bucket {
                grants: vec![(AUTHENTICATED, "FULL_CONTROL")],
                ..Bucket::clean()
            },
        ),
        (
            "private",
            Bucket {
                grants: vec![("http://acs.amazonaws.com/groups/s3/LogDelivery", "WRITE")],
                ..Bucket::clean()
            },
        ),
    ]);
    let report = scan(&mock, &["readable", "signedin", "signedinwrite", "private"]).await;
    let severity = |i: usize| report.buckets[i].findings.first().map(|f| f.severity);
    assert_eq!(severity(0), Some(Severity::High));
    assert_eq!(severity(1), Some(Severity::Medium));
    assert_eq!(severity(2), Some(Severity::High));
    assert_eq!(severity(3), None);
}

#[tokio::test]
async fn partial_public_access_block_lists_the_missing_toggles() {
    let mock = server(&[(
        "partial",
        Bucket {
            block: Some([true, true, false, false]),
            ..Bucket::clean()
        },
    )]);
    let report = scan(&mock, &["partial"]).await;
    assert_eq!(ids(&report, 0), [FindingId::PublicAccessBlockPartial]);
    let finding = &report.buckets[0].findings[0];
    assert_eq!(finding.severity, Severity::Medium);
    assert!(finding.explanation.contains("Block public policy"));
    assert!(finding.explanation.contains("Restrict public buckets"));
    assert!(!finding.explanation.contains("Block public ACLs"));
}

#[tokio::test]
async fn suspended_versioning_and_enabled_acls_are_low() {
    let mock = server(&[(
        "old",
        Bucket {
            versioning: "Suspended",
            ownership: None,
            ..Bucket::clean()
        },
    )]);
    let report = scan(&mock, &["old"]).await;
    assert_eq!(
        ids(&report, 0),
        [FindingId::VersioningSuspended, FindingId::AclsEnabled]
    );
    assert!(
        report.buckets[0]
            .findings
            .iter()
            .all(|f| f.severity == Severity::Low)
    );
}

#[tokio::test]
async fn a_public_policy_status_wins() {
    let mock = server(&[(
        "site",
        Bucket {
            policy_public: Some(true),
            ..Bucket::clean()
        },
    )]);
    let report = scan(&mock, &["site"]).await;
    assert_eq!(ids(&report, 0), [FindingId::PublicPolicy]);
    // The policy text is not even fetched when the status is known.
    assert!(
        mock.requests()
            .iter()
            .all(|r| !r.query.starts_with("policy&") && r.query != "policy")
    );
}

const PUBLIC_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"*","Action":"s3:GetObject","Resource":"arn:aws:s3:::site/*"}]}"#;
const CONDITIONAL_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"*","Action":"s3:GetObject","Resource":"arn:aws:s3:::site/*","Condition":{"IpAddress":{"aws:SourceIp":"10.0.0.0/8"}}}]}"#;

#[tokio::test]
async fn without_policy_status_the_policy_text_is_read_conservatively() {
    let mock = server(&[
        (
            "open",
            Bucket {
                policy_status: Answer::Unsupported,
                policy: Some(PUBLIC_POLICY),
                ..Bucket::clean()
            },
        ),
        (
            "limited",
            Bucket {
                policy_status: Answer::Unsupported,
                policy: Some(CONDITIONAL_POLICY),
                ..Bucket::clean()
            },
        ),
        (
            "nopolicy",
            Bucket {
                policy_status: Answer::Unsupported,
                policy: None,
                ..Bucket::clean()
            },
        ),
    ]);
    let report = scan(&mock, &["open", "limited", "nopolicy"]).await;
    assert_eq!(ids(&report, 0), [FindingId::PublicPolicy]);
    assert!(ids(&report, 1).is_empty());
    assert!(ids(&report, 2).is_empty());
    assert!(report.buckets.iter().all(|b| b.skipped.is_empty()));
}

#[tokio::test]
async fn a_refused_check_is_skipped_and_the_others_still_report() {
    let mock = server(&[(
        "locked",
        Bucket {
            acl: Answer::Denied,
            block_answer: Answer::Unsupported,
            encrypted: false,
            ..Bucket::clean()
        },
    )]);
    let report = scan(&mock, &["locked"]).await;
    let scan = &report.buckets[0];
    assert_eq!(ids(&report, 0), [FindingId::NoDefaultEncryption]);
    assert!(scan.error.is_none());
    let checks: Vec<Check> = scan.skipped.iter().map(|s| s.check).collect();
    assert_eq!(checks, [Check::Acl, Check::PublicAccessBlock]);
    assert!(!scan.skipped[0].unsupported);
    assert!(scan.skipped[0].message.contains("bucket ACL"));
    assert!(scan.skipped[1].unsupported);
}

#[tokio::test]
async fn a_bad_bucket_does_not_stop_the_scan() {
    let mock = server(&[("good", Bucket::bad()), ("alsogood", Bucket::clean())]);
    let report = scan(&mock, &["good", "bad name", "alsogood", "good"]).await;
    assert_eq!(report.buckets.len(), 3, "duplicates are scanned once");
    assert_eq!(report.buckets[0].bucket, "good");
    assert!(!report.buckets[0].findings.is_empty());
    let error = report.buckets[1].error.as_ref().unwrap();
    assert_eq!(error.code, ErrorKind::Usage);
    assert!(report.buckets[1].findings.is_empty());
    assert!(report.buckets[2].findings.is_empty());
    assert_eq!(report.failed_buckets(), 1);
}

#[tokio::test]
async fn the_scan_only_reads() {
    let mock = server(&[("messy", Bucket::bad())]);
    scan(&mock, &["messy"]).await;
    let requests = mock.requests();
    assert!(!requests.is_empty());
    assert!(
        requests
            .iter()
            .all(|r| r.method == "GET" && r.body.is_empty())
    );
}
