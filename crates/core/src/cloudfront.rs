//! CloudFront distributions and invalidations (task T09).
//!
//! [`CloudFront`] wraps the SDK client of one account: list, read, create,
//! edit, enable and disable distributions, find the distributions that serve
//! a bucket, detach a bucket from a distribution, create invalidations and
//! read their history.
//!
//! # Client
//!
//! CloudFront is a global service: requests are signed for `us-east-1` (or
//! `cn-northwest-1` for an account in the China partition) whatever the
//! region of the account. The client uses the account's credentials (the
//! temporary ones of an MFA session when there is one), the proxy-aware HTTP
//! client of [`crate::proxy`], the connect and read timeouts of
//! [`ClientOptions`] and the standard retry policy. It is never built for an
//! account with a custom endpoint: such a provider has no CloudFront, and
//! sending its keys to AWS would expose them to a third party.
//!
//! # Editing and ETags
//!
//! Every change is a read-modify-write of the distribution configuration:
//! `GetDistributionConfig`, the change, `UpdateDistribution` with `If-Match`
//! set to the ETag that was read. The caller may pass the ETag it showed the
//! user (`expected_etag`): when the distribution has changed since, nothing
//! is sent and the call fails with `conflict`, so one person's edit never
//! silently overwrites another's. A `PreconditionFailed` answer from the
//! service (someone changed it between the read and the update) is a
//! `conflict` too. Everything the editor does not manage (behaviours, custom
//! errors, certificates, logging, ...) is sent back unchanged.
//!
//! # Buckets
//!
//! A distribution serves a bucket when one of its origins has an S3 domain
//! name of that bucket (`bucket.s3.amazonaws.com`,
//! `bucket.s3.<region>.amazonaws.com`, a website endpoint, ...); see
//! [`origin_bucket`]. [`distributions_for_bucket`] filters a list, and
//! [`buckets_with_distributions`] is what marks buckets with the
//! `cloudfront` badge.
//!
//! # Invalidation paths
//!
//! [`validate_paths`]: every path starts with `/`, `*` may only be the last
//! character, there are at most [`MAX_INVALIDATION_PATHS`] paths and no
//! character is blank or a control character (spaces must be sent as `%20`).
//! Saved path sets ([`save_path_set`]) live in `config.toml`.

use std::collections::{BTreeSet, HashSet};
use std::time::Duration;

use aws_sdk_cloudfront::config::retry::RetryConfig;
use aws_sdk_cloudfront::config::timeout::TimeoutConfig;
use aws_sdk_cloudfront::config::{BehaviorVersion, Region, SharedHttpClient};
use aws_sdk_cloudfront::types::{
    Aliases, DefaultCacheBehavior, DistributionConfig, DistributionSummary, InvalidationBatch,
    InvalidationSummary, Origin, OriginAccessControlConfig, OriginAccessControlOriginTypes,
    OriginAccessControlSigningBehaviors, OriginAccessControlSigningProtocols, Origins, Paths,
    PriceClass, S3OriginConfig, ViewerProtocolPolicy,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::client::{ClientOptions, Credentials};
use crate::config::{ConfigStore, new_uuid_v4};
use crate::error::{Error, Result};
use crate::model::Account;
use crate::ops::{SeenTokens, to_utc};

/// Most paths one invalidation may hold (the service limit).
pub const MAX_INVALIDATION_PATHS: usize = 3000;

/// Longest invalidation path, in bytes (the service limit).
pub const MAX_PATH_BYTES: usize = 4000;

/// Longest comment of a distribution, in characters (the service limit).
pub const MAX_COMMENT_CHARS: usize = 128;

/// Most saved invalidation path sets.
pub const MAX_PATH_SETS: usize = 100;

/// Longest name of a path set, in characters.
pub const MAX_SET_NAME_CHARS: usize = 100;

/// Items per request when everything is listed.
const PAGE_ITEMS: i32 = 100;

/// Most pages one complete listing follows (a guard against a service that
/// never ends its listing).
const MAX_PAGES: usize = 500;

/// Items of one page of the invalidation history.
pub const HISTORY_PAGE: u32 = 50;

/// AWS managed cache policy "CachingOptimized", used by new distributions.
const CACHING_OPTIMIZED: &str = "658327ea-f89d-4fab-a63d-7e88639e58f6";

/// The price classes a distribution can have, widest first.
pub const PRICE_CLASSES: [&str; 3] = ["PriceClass_All", "PriceClass_200", "PriceClass_100"];

/// Timeout of the whole client request set (CloudFront answers metadata
/// calls quickly; this only bounds a hung connection).
const OPERATION_TIMEOUT: Duration = Duration::from_secs(120);

/// One origin of a distribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistributionOrigin {
    /// Origin id, as the behaviours name it.
    pub id: String,
    /// Domain name of the origin.
    pub domain_name: String,
    /// Path prefix added to every request to the origin (empty for none).
    pub origin_path: String,
    /// The S3 bucket the domain name belongs to, when it is an S3 domain.
    pub bucket: Option<String>,
    /// The domain is an S3 website endpoint.
    pub website: bool,
    /// The origin is signed with an origin access control.
    pub access_control: bool,
}

/// A distribution, as listed or read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Distribution {
    /// Distribution id, such as `E2ABCDEFGHIJKL`.
    pub id: String,
    /// ARN.
    pub arn: String,
    /// The `*.cloudfront.net` domain name.
    pub domain_name: String,
    /// `Deployed` or `InProgress`.
    pub status: String,
    /// Whether the distribution accepts requests.
    pub enabled: bool,
    /// Free text.
    pub comment: String,
    /// Alternate domain names.
    pub aliases: Vec<String>,
    /// `PriceClass_All`, `PriceClass_200` or `PriceClass_100`.
    pub price_class: String,
    /// When the distribution was last changed.
    pub last_modified: Option<DateTime<Utc>>,
    /// The origins.
    pub origins: Vec<DistributionOrigin>,
    /// Object served for the root URL; `None` in a listing, which does not
    /// carry it.
    pub default_root_object: Option<String>,
    /// Invalidation batches in progress; `None` in a listing.
    pub in_progress_invalidations: Option<u32>,
    /// ETag of the configuration; `None` in a listing. Pass it back as
    /// `expected_etag` to detect changes made meanwhile.
    pub etag: Option<String>,
}

/// One page of a distribution listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistributionPage {
    /// The distributions of the page.
    pub items: Vec<Distribution>,
    /// Marker of the next page; `None` after the last.
    pub next_marker: Option<String>,
}

/// An invalidation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invalidation {
    /// Invalidation id.
    pub id: String,
    /// `InProgress` or `Completed`.
    pub status: String,
    /// When it was created.
    pub create_time: Option<DateTime<Utc>>,
    /// The paths; empty in a history listing, which does not carry them (read
    /// one with [`CloudFront::get_invalidation`]).
    pub paths: Vec<String>,
}

/// One page of the invalidation history, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvalidationPage {
    /// The invalidations of the page.
    pub items: Vec<Invalidation>,
    /// Marker of the next page; `None` after the last.
    pub next_marker: Option<String>,
}

/// What a new distribution for a bucket is made of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewDistribution {
    /// The S3 bucket to serve.
    pub bucket: String,
    /// Region of the bucket; the origin is the regional endpoint.
    pub region: String,
    /// Comment (at most [`MAX_COMMENT_CHARS`] characters).
    pub comment: String,
    /// Whether the distribution starts enabled.
    pub enabled: bool,
    /// Object served for the root URL, without a leading `/`; empty for none.
    pub default_root_object: String,
    /// Price class; `None` is `PriceClass_All`.
    pub price_class: Option<String>,
}

/// A created distribution and what has to be done in the bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedDistribution {
    /// The new distribution.
    pub distribution: Distribution,
    /// Id of the origin access control that was created for it.
    pub origin_access_control_id: String,
    /// A bucket policy that lets this distribution (and only it) read the
    /// bucket. The bucket is not changed: add the statement to its policy.
    pub bucket_policy: String,
}

/// Changes to a distribution; a `None` field stays as it is.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DistributionEdit {
    /// New comment.
    pub comment: Option<String>,
    /// New alternate domain names (replaces the list; empty removes them).
    pub aliases: Option<Vec<String>>,
    /// New default root object (empty removes it).
    pub default_root_object: Option<String>,
    /// New price class.
    pub price_class: Option<String>,
    /// Enable or disable.
    pub enabled: Option<bool>,
}

/// A named list of invalidation paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathSet {
    /// Name, unique ignoring case.
    pub name: String,
    /// The paths, valid per [`validate_paths`].
    pub paths: Vec<String>,
}

/// The region CloudFront requests are signed for: the global one, or the
/// China one for an account of that partition.
pub fn signing_region(account: &Account) -> &'static str {
    if account.region.starts_with("cn-") {
        "cn-northwest-1"
    } else {
        "us-east-1"
    }
}

/// CloudFront client configuration for `account`: credentials, region,
/// retries and timeouts as described in the module documentation. Callers
/// add the HTTP client before building. Fails with `usage` for an account
/// with a custom endpoint.
pub fn cloudfront_config(
    account: &Account,
    credentials: &Credentials,
    options: &ClientOptions,
) -> Result<aws_sdk_cloudfront::config::Builder> {
    let static_credentials = aws_sdk_cloudfront::config::Credentials::new(
        credentials.access_key_id.as_str(),
        credentials.secret_access_key.as_str(),
        credentials.session_token.clone(),
        None,
        "bucketbridge-account",
    );
    cloudfront_config_with_provider(
        account,
        aws_credential_types::provider::SharedCredentialsProvider::new(static_credentials),
        options,
    )
}

/// [`cloudfront_config`] signed by a refreshing credentials provider (see
/// [`crate::creds`]).
pub fn cloudfront_config_with_provider(
    account: &Account,
    provider: aws_credential_types::provider::SharedCredentialsProvider,
    options: &ClientOptions,
) -> Result<aws_sdk_cloudfront::config::Builder> {
    check_account(account)?;
    if options.max_attempts == 0 {
        return Err(Error::Usage("max_attempts must be at least 1".into()));
    }
    Ok(aws_sdk_cloudfront::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new(signing_region(account)))
        .credentials_provider(provider)
        .retry_config(RetryConfig::standard().with_max_attempts(options.max_attempts))
        .timeout_config(
            TimeoutConfig::builder()
                .connect_timeout(options.connect_timeout)
                .read_timeout(options.read_timeout)
                .operation_timeout(OPERATION_TIMEOUT)
                .build(),
        ))
}

/// Fails with `usage` for an account with a custom endpoint, which has no
/// CloudFront.
pub fn check_account(account: &Account) -> Result<()> {
    if account.endpoint.is_some() {
        return Err(Error::Usage(
            "CloudFront is not available for an account with a custom endpoint".into(),
        ));
    }
    Ok(())
}

/// A CloudFront handle for one account.
#[derive(Debug, Clone)]
pub struct CloudFront {
    client: aws_sdk_cloudfront::Client,
}

impl CloudFront {
    /// Builds the client for `account` (see [`cloudfront_config`]); pass the
    /// proxy-aware client of [`crate::proxy`] as `http_client`.
    pub fn new(
        account: &Account,
        credentials: &Credentials,
        options: &ClientOptions,
        http_client: Option<SharedHttpClient>,
    ) -> Result<Self> {
        let mut builder = cloudfront_config(account, credentials, options)?;
        if let Some(http_client) = http_client {
            builder = builder.http_client(http_client);
        }
        Ok(Self::from_client(aws_sdk_cloudfront::Client::from_conf(
            builder.build(),
        )))
    }

    /// Wraps an existing client (tests).
    pub fn from_client(client: aws_sdk_cloudfront::Client) -> Self {
        CloudFront { client }
    }

    // ---- distributions ----

    /// One page of distributions after `marker` (`None` starts the listing).
    pub async fn list_distributions_page(&self, marker: Option<&str>) -> Result<DistributionPage> {
        let out = self
            .client
            .list_distributions()
            .set_marker(marker.map(str::to_owned))
            .max_items(PAGE_ITEMS)
            .send()
            .await
            .map_err(|err| cf_error(err, "listing distributions"))?;
        let Some(list) = out.distribution_list() else {
            return Ok(DistributionPage {
                items: Vec::new(),
                next_marker: None,
            });
        };
        let next_marker = if list.is_truncated() {
            list.next_marker()
                .filter(|marker| !marker.is_empty())
                .map(str::to_owned)
        } else {
            None
        };
        Ok(DistributionPage {
            items: list.items().iter().map(summary_to_model).collect(),
            next_marker,
        })
    }

    /// Every distribution of the account, following the markers. A marker
    /// the service repeats stops the listing with a `remote` error.
    pub async fn list_all_distributions(&self) -> Result<Vec<Distribution>> {
        let mut all = Vec::new();
        let mut marker: Option<String> = None;
        let mut seen = SeenTokens::new();
        for _ in 0..MAX_PAGES {
            let page = self.list_distributions_page(marker.as_deref()).await?;
            all.extend(page.items);
            match page.next_marker {
                None => return Ok(all),
                Some(next) if !seen.first_time(next.as_str()) => {
                    return Err(Error::Remote(
                        "listing distributions: the service returned the same marker twice; \
                         stopping to avoid an endless listing"
                            .into(),
                    ));
                }
                Some(next) => marker = Some(next),
            }
        }
        Err(Error::Remote(format!(
            "listing distributions: more than {MAX_PAGES} pages; stopping"
        )))
    }

    /// The distributions that have an origin in `bucket`.
    pub async fn distributions_for_bucket(&self, bucket: &str) -> Result<Vec<Distribution>> {
        let all = self.list_all_distributions().await?;
        Ok(distributions_for_bucket(&all, bucket))
    }

    /// One distribution with its ETag, default root object and the number of
    /// invalidation batches in progress.
    pub async fn get_distribution(&self, id: &str) -> Result<Distribution> {
        check_id(id)?;
        let out = self
            .client
            .get_distribution()
            .id(id)
            .send()
            .await
            .map_err(|err| cf_error(err, format!("reading distribution {id}")))?;
        let distribution = out
            .distribution()
            .ok_or_else(|| Error::Remote("CloudFront returned no distribution".into()))?;
        let config = distribution.distribution_config();
        Ok(Distribution {
            id: distribution.id().to_owned(),
            arn: distribution.arn().to_owned(),
            domain_name: distribution.domain_name().to_owned(),
            status: distribution.status().to_owned(),
            enabled: config.is_some_and(DistributionConfig::enabled),
            comment: config.map(|c| c.comment().to_owned()).unwrap_or_default(),
            aliases: config.map(config_aliases).unwrap_or_default(),
            price_class: config
                .and_then(|c| c.price_class())
                .map(|class| class.as_str().to_owned())
                .unwrap_or_else(|| PRICE_CLASSES[0].to_owned()),
            last_modified: to_utc(distribution.last_modified_time()),
            origins: config.map(config_origins).unwrap_or_default(),
            default_root_object: Some(
                config
                    .and_then(|c| c.default_root_object())
                    .unwrap_or_default()
                    .to_owned(),
            ),
            in_progress_invalidations: u32::try_from(
                distribution.in_progress_invalidation_batches(),
            )
            .ok(),
            etag: out.e_tag().map(str::to_owned),
        })
    }

    /// Creates a distribution for a bucket: an origin access control, the
    /// regional bucket endpoint as the only origin, and a default cache
    /// behaviour that redirects to HTTPS, compresses and uses the managed
    /// `CachingOptimized` policy. Returns the distribution and the bucket
    /// policy that lets it read the bucket (the bucket is not changed).
    pub async fn create_distribution(&self, new: &NewDistribution) -> Result<CreatedDistribution> {
        let plan = validate_new(new)?;
        let control = OriginAccessControlConfig::builder()
            .name(format!(
                "bb-{}-{}",
                short_name(&new.bucket),
                unique_suffix()?
            ))
            .description(format!("BucketBridge access to {}", new.bucket))
            .origin_access_control_origin_type(OriginAccessControlOriginTypes::S3)
            .signing_behavior(OriginAccessControlSigningBehaviors::Always)
            .signing_protocol(OriginAccessControlSigningProtocols::Sigv4)
            .build()
            .map_err(build_error)?;
        let control = self
            .client
            .create_origin_access_control()
            .origin_access_control_config(control)
            .send()
            .await
            .map_err(|err| cf_error(err, "creating the origin access control"))?;
        let control_id = control
            .origin_access_control()
            .map(|control| control.id().to_owned())
            .ok_or_else(|| Error::Remote("CloudFront returned no origin access control".into()))?;

        let origin_id = format!("s3-{}", new.bucket);
        let origin = Origin::builder()
            .id(&origin_id)
            .domain_name(plan.domain)
            .origin_access_control_id(&control_id)
            .s3_origin_config(S3OriginConfig::builder().origin_access_identity("").build())
            .build()
            .map_err(build_error)?;
        let behavior = DefaultCacheBehavior::builder()
            .target_origin_id(&origin_id)
            .viewer_protocol_policy(ViewerProtocolPolicy::RedirectToHttps)
            .compress(true)
            .cache_policy_id(CACHING_OPTIMIZED)
            .build()
            .map_err(build_error)?;
        let config = DistributionConfig::builder()
            .caller_reference(new_uuid_v4()?)
            .comment(plan.comment)
            .enabled(new.enabled)
            .default_root_object(plan.default_root_object)
            .price_class(plan.price_class)
            .origins(
                Origins::builder()
                    .quantity(1)
                    .items(origin)
                    .build()
                    .map_err(build_error)?,
            )
            .default_cache_behavior(behavior)
            .build()
            .map_err(build_error)?;
        let out = self
            .client
            .create_distribution()
            .distribution_config(config)
            .send()
            .await
            .map_err(|err| {
                cf_error(err, format!("creating a distribution for '{}'", new.bucket))
            })?;
        let created = out
            .distribution()
            .ok_or_else(|| Error::Remote("CloudFront returned no distribution".into()))?;
        let mut distribution = self.get_distribution(created.id()).await?;
        if distribution.etag.is_none() {
            distribution.etag = out.e_tag().map(str::to_owned);
        }
        let bucket_policy = oac_bucket_policy(&new.bucket, &distribution.arn);
        Ok(CreatedDistribution {
            distribution,
            origin_access_control_id: control_id,
            bucket_policy,
        })
    }

    /// Applies `edit` to a distribution and returns it as it is now. See the
    /// module documentation for `expected_etag`.
    pub async fn update_distribution(
        &self,
        id: &str,
        edit: &DistributionEdit,
        expected_etag: Option<&str>,
    ) -> Result<Distribution> {
        check_id(id)?;
        let edit = validate_edit(edit)?;
        let (mut config, etag) = self.read_config(id, expected_etag).await?;
        apply_edit(&mut config, &edit)?;
        self.write_config(id, config, etag).await?;
        self.get_distribution(id).await
    }

    /// Enables or disables a distribution.
    pub async fn set_enabled(
        &self,
        id: &str,
        enabled: bool,
        expected_etag: Option<&str>,
    ) -> Result<Distribution> {
        let edit = DistributionEdit {
            enabled: Some(enabled),
            ..DistributionEdit::default()
        };
        self.update_distribution(id, &edit, expected_etag).await
    }

    /// Removes the origins of `bucket` from a distribution (it stops serving
    /// the bucket; nothing is deleted). Refused with `conflict` when the
    /// distribution would be left without an origin, or when a behaviour or
    /// an origin group still uses one of the origins.
    pub async fn detach_bucket(
        &self,
        id: &str,
        bucket: &str,
        expected_etag: Option<&str>,
    ) -> Result<Distribution> {
        check_id(id)?;
        let (mut config, etag) = self.read_config(id, expected_etag).await?;
        detach_origins(&mut config, bucket)?;
        self.write_config(id, config, etag).await?;
        self.get_distribution(id).await
    }

    async fn read_config(
        &self,
        id: &str,
        expected_etag: Option<&str>,
    ) -> Result<(DistributionConfig, String)> {
        let out = self
            .client
            .get_distribution_config()
            .id(id)
            .send()
            .await
            .map_err(|err| cf_error(err, format!("reading the configuration of {id}")))?;
        let etag = out
            .e_tag()
            .filter(|etag| !etag.is_empty())
            .ok_or_else(|| Error::Remote("CloudFront returned no ETag".into()))?
            .to_owned();
        let config = out
            .distribution_config()
            .cloned()
            .ok_or_else(|| Error::Remote("CloudFront returned no configuration".into()))?;
        if let Some(expected) = expected_etag
            && expected != etag
        {
            return Err(changed_elsewhere(id));
        }
        Ok((config, etag))
    }

    async fn write_config(&self, id: &str, config: DistributionConfig, etag: String) -> Result<()> {
        self.client
            .update_distribution()
            .id(id)
            .distribution_config(config)
            .if_match(etag)
            .send()
            .await
            .map_err(|err| cf_error(err, format!("updating distribution {id}")))?;
        Ok(())
    }

    // ---- invalidations ----

    /// Creates an invalidation of `paths` (see [`validate_paths`]).
    pub async fn create_invalidation(
        &self,
        distribution_id: &str,
        paths: &[String],
    ) -> Result<Invalidation> {
        check_id(distribution_id)?;
        let paths = validate_paths(paths)?;
        let quantity = i32::try_from(paths.len())
            .map_err(|_| Error::Usage("too many invalidation paths".into()))?;
        let batch = InvalidationBatch::builder()
            .paths(
                Paths::builder()
                    .quantity(quantity)
                    .set_items(Some(paths))
                    .build()
                    .map_err(build_error)?,
            )
            .caller_reference(new_uuid_v4()?)
            .build()
            .map_err(build_error)?;
        let out = self
            .client
            .create_invalidation()
            .distribution_id(distribution_id)
            .invalidation_batch(batch)
            .send()
            .await
            .map_err(|err| cf_error(err, format!("invalidating {distribution_id}")))?;
        let invalidation = out
            .invalidation()
            .ok_or_else(|| Error::Remote("CloudFront returned no invalidation".into()))?;
        Ok(Invalidation {
            id: invalidation.id().to_owned(),
            status: invalidation.status().to_owned(),
            create_time: to_utc(invalidation.create_time()),
            paths: invalidation
                .invalidation_batch()
                .and_then(|batch| batch.paths())
                .map(|paths| paths.items().to_vec())
                .unwrap_or_default(),
        })
    }

    /// One invalidation with its paths.
    pub async fn get_invalidation(
        &self,
        distribution_id: &str,
        invalidation_id: &str,
    ) -> Result<Invalidation> {
        check_id(distribution_id)?;
        check_id(invalidation_id)?;
        let out = self
            .client
            .get_invalidation()
            .distribution_id(distribution_id)
            .id(invalidation_id)
            .send()
            .await
            .map_err(|err| cf_error(err, format!("reading invalidation {invalidation_id}")))?;
        let invalidation = out
            .invalidation()
            .ok_or_else(|| Error::Remote("CloudFront returned no invalidation".into()))?;
        Ok(Invalidation {
            id: invalidation.id().to_owned(),
            status: invalidation.status().to_owned(),
            create_time: to_utc(invalidation.create_time()),
            paths: invalidation
                .invalidation_batch()
                .and_then(|batch| batch.paths())
                .map(|paths| paths.items().to_vec())
                .unwrap_or_default(),
        })
    }

    /// One page of the invalidation history, newest first, [`HISTORY_PAGE`]
    /// entries at most; `marker` is the `next_marker` of the previous page.
    pub async fn list_invalidations(
        &self,
        distribution_id: &str,
        marker: Option<&str>,
    ) -> Result<InvalidationPage> {
        check_id(distribution_id)?;
        let out = self
            .client
            .list_invalidations()
            .distribution_id(distribution_id)
            .set_marker(marker.map(str::to_owned))
            .max_items(HISTORY_PAGE as i32)
            .send()
            .await
            .map_err(|err| {
                cf_error(
                    err,
                    format!("listing the invalidations of {distribution_id}"),
                )
            })?;
        let Some(list) = out.invalidation_list() else {
            return Ok(InvalidationPage {
                items: Vec::new(),
                next_marker: None,
            });
        };
        let next_marker = if list.is_truncated() {
            list.next_marker()
                .filter(|marker| !marker.is_empty())
                .map(str::to_owned)
        } else {
            None
        };
        Ok(InvalidationPage {
            items: list.items().iter().map(invalidation_summary).collect(),
            next_marker,
        })
    }
}

// ---- pure rules ----

/// Checks invalidation paths and returns them in the form that is sent:
/// trimmed of surrounding blanks and without repeats (first occurrence
/// kept). Fails with `usage` listing every problem found.
///
/// A path starts with `/`; `*` is allowed only as the last character (it
/// matches everything below, `/*` invalidates all); no blank or control
/// character is allowed inside (send a space as `%20`); a path is at most
/// [`MAX_PATH_BYTES`] bytes and there are 1 to [`MAX_INVALIDATION_PATHS`] of
/// them.
pub fn validate_paths(paths: &[String]) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    let mut checked: Vec<String> = Vec::new();
    for (index, raw) in paths.iter().enumerate() {
        let path = raw.trim();
        if path.is_empty() {
            problems.push(format!("path {} is empty", index + 1));
            continue;
        }
        if let Some(problem) = path_problem(path) {
            problems.push(format!("'{}': {problem}", path.escape_debug()));
            continue;
        }
        if !checked.iter().any(|seen| seen == path) {
            checked.push(path.to_owned());
        }
    }
    if checked.is_empty() && problems.is_empty() {
        problems.push("give at least one path".into());
    }
    if checked.len() > MAX_INVALIDATION_PATHS {
        problems.push(format!(
            "{} paths; one invalidation holds at most {MAX_INVALIDATION_PATHS}",
            checked.len()
        ));
    }
    if problems.is_empty() {
        Ok(checked)
    } else {
        Err(Error::Usage(format!(
            "invalid invalidation paths: {}",
            problems.join("; ")
        )))
    }
}

fn path_problem(path: &str) -> Option<String> {
    if !path.starts_with('/') {
        return Some("a path must start with '/'".into());
    }
    if path.len() > MAX_PATH_BYTES {
        return Some(format!("longer than {MAX_PATH_BYTES} bytes"));
    }
    if path.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Some("blanks and control characters must be percent-encoded (%20)".into());
    }
    if let Some(at) = path.find('*')
        && at != path.len() - 1
    {
        return Some("'*' is only allowed as the last character".into());
    }
    None
}

/// The bucket an origin domain name belongs to and whether it is a website
/// endpoint, or `None` when the domain is not an S3 one. The name is
/// lowercased. `bucket.s3.amazonaws.com`, `bucket.s3.<region>.amazonaws.com`,
/// `bucket.s3-<region>.amazonaws.com`, dual-stack names and
/// `bucket.s3-website-<region>.amazonaws.com` are recognised, in the China
/// partition too.
pub fn origin_bucket(domain: &str) -> Option<(String, bool)> {
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    if !(domain.ends_with(".amazonaws.com") || domain.ends_with(".amazonaws.com.cn")) {
        return None;
    }
    // The rightmost ".s3" starts the service part: a bucket name may contain
    // dots, and even ".s3".
    let mut start = None;
    for (at, _) in domain.match_indices(".s3") {
        let after = domain[at + 3..].chars().next();
        if matches!(after, Some('.' | '-')) {
            start = Some(at);
        }
    }
    let at = start?;
    if at == 0 {
        return None;
    }
    let website = domain[at..].starts_with(".s3-website");
    Some((domain[..at].to_owned(), website))
}

/// The distributions of `all` with an origin in `bucket` (compared ignoring
/// case).
pub fn distributions_for_bucket(all: &[Distribution], bucket: &str) -> Vec<Distribution> {
    let bucket = bucket.to_ascii_lowercase();
    all.iter()
        .filter(|distribution| {
            distribution
                .origins
                .iter()
                .any(|origin| origin.bucket.as_deref() == Some(bucket.as_str()))
        })
        .cloned()
        .collect()
}

/// The names of `buckets` that have a distribution in `all`.
pub fn buckets_with_distributions(all: &[Distribution], buckets: &[String]) -> HashSet<String> {
    let served: HashSet<&str> = all
        .iter()
        .flat_map(|distribution| distribution.origins.iter())
        .filter_map(|origin| origin.bucket.as_deref())
        .collect();
    buckets
        .iter()
        .filter(|bucket| served.contains(bucket.to_ascii_lowercase().as_str()))
        .cloned()
        .collect()
}

/// The bucket policy statement that lets one distribution read a bucket
/// through an origin access control.
pub fn oac_bucket_policy(bucket: &str, distribution_arn: &str) -> String {
    let partition = distribution_arn
        .split(':')
        .nth(1)
        .filter(|partition| partition.starts_with("aws"))
        .unwrap_or("aws");
    let policy = serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowCloudFrontServicePrincipalReadOnly",
            "Effect": "Allow",
            "Principal": { "Service": "cloudfront.amazonaws.com" },
            "Action": "s3:GetObject",
            "Resource": format!("arn:{partition}:s3:::{bucket}/*"),
            "Condition": { "StringEquals": { "AWS:SourceArn": distribution_arn } },
        }],
    });
    serde_json::to_string_pretty(&policy).unwrap_or_default()
}

struct NewPlan {
    domain: String,
    comment: String,
    default_root_object: String,
    price_class: PriceClass,
}

fn validate_new(new: &NewDistribution) -> Result<NewPlan> {
    crate::s3uri::validate_bucket_name(&new.bucket)?;
    crate::client::validate_region(&new.region)?;
    let comment = check_comment(&new.comment)?;
    let default_root_object = check_root_object(&new.default_root_object)?;
    let price_class = match new.price_class.as_deref() {
        None => PriceClass::PriceClassAll,
        Some(class) => check_price_class(class)?,
    };
    let suffix = if new.region.starts_with("cn-") {
        "amazonaws.com.cn"
    } else {
        "amazonaws.com"
    };
    Ok(NewPlan {
        domain: format!("{}.s3.{}.{suffix}", new.bucket, new.region),
        comment,
        default_root_object,
        price_class,
    })
}

fn check_comment(comment: &str) -> Result<String> {
    let comment = comment.trim();
    if comment.chars().count() > MAX_COMMENT_CHARS {
        return Err(Error::Usage(format!(
            "the comment is longer than {MAX_COMMENT_CHARS} characters"
        )));
    }
    if comment.chars().any(char::is_control) {
        return Err(Error::Usage(
            "the comment must not contain control characters".into(),
        ));
    }
    Ok(comment.to_owned())
}

fn check_root_object(object: &str) -> Result<String> {
    let object = object.trim();
    if object.starts_with('/') {
        return Err(Error::Usage(
            "the default root object must not start with '/'".into(),
        ));
    }
    if object.chars().any(|c| c.is_control() || c.is_whitespace()) || object.contains('?') {
        return Err(Error::Usage(
            "the default root object must not contain blanks, control characters or '?'".into(),
        ));
    }
    Ok(object.to_owned())
}

fn check_price_class(class: &str) -> Result<PriceClass> {
    if !PRICE_CLASSES.contains(&class) {
        return Err(Error::Usage(format!(
            "unknown price class '{class}'; use one of {}",
            PRICE_CLASSES.join(", ")
        )));
    }
    Ok(PriceClass::from(class))
}

/// Checks an alternate domain name: dot separated labels of letters, digits
/// and hyphens (a leading `*.` wildcard is allowed), at most 253 characters.
pub fn check_alias(alias: &str) -> Result<()> {
    let bad = || Error::Usage(format!("'{alias}' is not a valid domain name"));
    let name = alias.strip_prefix("*.").unwrap_or(alias);
    if name.is_empty() || alias.len() > 253 || !name.contains('.') {
        return Err(bad());
    }
    for label in name.split('.') {
        let valid = !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if !valid {
            return Err(bad());
        }
    }
    Ok(())
}

fn validate_edit(edit: &DistributionEdit) -> Result<DistributionEdit> {
    let aliases = match &edit.aliases {
        None => None,
        Some(list) => {
            let mut out: Vec<String> = Vec::new();
            for alias in list {
                let alias = alias.trim().to_ascii_lowercase();
                check_alias(&alias)?;
                if !out.contains(&alias) {
                    out.push(alias);
                }
            }
            Some(out)
        }
    };
    Ok(DistributionEdit {
        comment: edit.comment.as_deref().map(check_comment).transpose()?,
        aliases,
        default_root_object: edit
            .default_root_object
            .as_deref()
            .map(check_root_object)
            .transpose()?,
        price_class: match edit.price_class.as_deref() {
            None => None,
            Some(class) => Some(check_price_class(class)?.as_str().to_owned()),
        },
        enabled: edit.enabled,
    })
}

fn apply_edit(config: &mut DistributionConfig, edit: &DistributionEdit) -> Result<()> {
    if let Some(comment) = &edit.comment {
        config.comment = comment.clone();
    }
    if let Some(aliases) = &edit.aliases {
        let quantity = i32::try_from(aliases.len())
            .map_err(|_| Error::Usage("too many alternate domain names".into()))?;
        config.aliases = Some(
            Aliases::builder()
                .quantity(quantity)
                .set_items(Some(aliases.clone()))
                .build()
                .map_err(build_error)?,
        );
    }
    if let Some(object) = &edit.default_root_object {
        config.default_root_object = Some(object.clone());
    }
    if let Some(class) = &edit.price_class {
        config.price_class = Some(PriceClass::from(class.as_str()));
    }
    if let Some(enabled) = edit.enabled {
        config.enabled = enabled;
    }
    Ok(())
}

fn detach_origins(config: &mut DistributionConfig, bucket: &str) -> Result<()> {
    let bucket = bucket.to_ascii_lowercase();
    let origins = config
        .origins
        .as_ref()
        .map(|origins| origins.items().to_vec())
        .unwrap_or_default();
    let (removed, kept): (Vec<Origin>, Vec<Origin>) = origins.into_iter().partition(|origin| {
        origin_bucket(origin.domain_name()).is_some_and(|(name, _)| name == bucket)
    });
    if removed.is_empty() {
        return Err(Error::NotFound(format!(
            "the distribution has no origin in bucket '{bucket}'"
        )));
    }
    if kept.is_empty() {
        return Err(Error::Conflict(format!(
            "bucket '{bucket}' is the only origin of the distribution; \
             a distribution needs one, so delete the distribution instead"
        )));
    }
    let removed_ids: BTreeSet<&str> = removed.iter().map(Origin::id).collect();
    let mut users: Vec<String> = Vec::new();
    if let Some(default) = &config.default_cache_behavior
        && removed_ids.contains(default.target_origin_id())
    {
        users.push("the default behaviour".into());
    }
    if let Some(behaviors) = &config.cache_behaviors {
        for behavior in behaviors.items() {
            if removed_ids.contains(behavior.target_origin_id()) {
                users.push(format!("the behaviour for '{}'", behavior.path_pattern()));
            }
        }
    }
    if let Some(groups) = &config.origin_groups {
        for group in groups.items() {
            let uses = group.members().is_some_and(|members| {
                members
                    .items()
                    .iter()
                    .any(|member| removed_ids.contains(member.origin_id()))
            });
            if uses {
                users.push(format!("the origin group '{}'", group.id()));
            }
        }
    }
    if !users.is_empty() {
        return Err(Error::Conflict(format!(
            "bucket '{bucket}' is still used by {}; change them first",
            users.join(", ")
        )));
    }
    let quantity = i32::try_from(kept.len()).map_err(|_| Error::Internal("origins".into()))?;
    config.origins = Some(
        Origins::builder()
            .quantity(quantity)
            .set_items(Some(kept))
            .build()
            .map_err(build_error)?,
    );
    Ok(())
}

// ---- SDK conversions ----

fn config_aliases(config: &DistributionConfig) -> Vec<String> {
    config
        .aliases()
        .map(|aliases| aliases.items().to_vec())
        .unwrap_or_default()
}

fn origin_to_model(origin: &Origin) -> DistributionOrigin {
    let bucket = origin_bucket(origin.domain_name());
    DistributionOrigin {
        id: origin.id().to_owned(),
        domain_name: origin.domain_name().to_owned(),
        origin_path: origin.origin_path().unwrap_or_default().to_owned(),
        website: bucket.as_ref().is_some_and(|(_, website)| *website),
        bucket: bucket.map(|(name, _)| name),
        access_control: origin
            .origin_access_control_id()
            .is_some_and(|id| !id.is_empty()),
    }
}

fn config_origins(config: &DistributionConfig) -> Vec<DistributionOrigin> {
    config
        .origins()
        .map(|origins| origins.items().iter().map(origin_to_model).collect())
        .unwrap_or_default()
}

fn summary_to_model(summary: &DistributionSummary) -> Distribution {
    Distribution {
        id: summary.id().to_owned(),
        arn: summary.arn().to_owned(),
        domain_name: summary.domain_name().to_owned(),
        status: summary.status().to_owned(),
        enabled: summary.enabled(),
        comment: summary.comment().to_owned(),
        aliases: summary
            .aliases()
            .map(|aliases| aliases.items().to_vec())
            .unwrap_or_default(),
        price_class: summary.price_class().as_str().to_owned(),
        last_modified: to_utc(summary.last_modified_time()),
        origins: summary
            .origins()
            .map(|origins| origins.items().iter().map(origin_to_model).collect())
            .unwrap_or_default(),
        default_root_object: None,
        in_progress_invalidations: None,
        etag: None,
    }
}

fn invalidation_summary(summary: &InvalidationSummary) -> Invalidation {
    Invalidation {
        id: summary.id().to_owned(),
        status: summary.status().to_owned(),
        create_time: to_utc(summary.create_time()),
        paths: Vec::new(),
    }
}

// ---- errors ----

fn changed_elsewhere(id: &str) -> Error {
    Error::Conflict(format!(
        "distribution {id} was changed since it was loaded; reload it and try again"
    ))
}

fn build_error(error: aws_sdk_cloudfront::error::BuildError) -> Error {
    Error::Internal(format!("building a CloudFront request: {error}"))
}

/// Converts an SDK failure: stale ETags are `conflict`, unknown
/// distributions and invalidations are `not_found`, other client mistakes
/// (HTTP 400) are `usage`, the rest follows [`Error::from`].
fn cf_error<E, R>(
    error: aws_smithy_runtime_api::client::result::SdkError<E, R>,
    context: impl std::fmt::Display,
) -> Error
where
    E: aws_smithy_types::error::metadata::ProvideErrorMetadata
        + std::error::Error
        + Send
        + Sync
        + 'static,
    R: std::fmt::Debug + Send + Sync + 'static,
{
    let converted = Error::from(error).with_context(&context);
    let (code, status) = match converted.service() {
        Some(service) => (
            service.code().unwrap_or_default().to_owned(),
            service.status(),
        ),
        None => return converted,
    };
    let message = converted.to_string();
    match code.as_str() {
        "PreconditionFailed" | "InvalidIfMatchVersion" => Error::Conflict(format!(
            "{message} (the configuration was changed elsewhere; reload and try again)"
        )),
        "NoSuchDistribution" | "NoSuchInvalidation" | "NoSuchOriginAccessControl" => {
            Error::NotFound(message)
        }
        "TooManyInvalidationsInProgress"
        | "DistributionNotDisabled"
        | "CNAMEAlreadyExists"
        | "DistributionAlreadyExists"
        | "IllegalUpdate"
        | "OriginAccessControlAlreadyExists" => Error::Conflict(message),
        "AccessDenied" | "AccessDeniedException" => converted,
        _ if status == Some(400) && !code.contains("Throttl") => Error::Usage(message),
        _ => converted,
    }
}

fn check_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(Error::Usage(format!("'{id}' is not a CloudFront id")));
    }
    Ok(())
}

fn short_name(bucket: &str) -> String {
    bucket.chars().take(40).collect()
}

fn unique_suffix() -> Result<String> {
    Ok(new_uuid_v4()?.chars().take(8).collect())
}

// ---- saved path sets ----

/// The saved path sets, in order of creation. Blocking.
pub fn list_path_sets(store: &ConfigStore) -> Result<Vec<PathSet>> {
    Ok(store.load()?.invalidation_path_sets)
}

/// Adds a set (`replaces` is `None`) or replaces the set named `replaces`,
/// keeping its place. Names are trimmed, 1 to [`MAX_SET_NAME_CHARS`]
/// characters, without control characters and unique ignoring case; the
/// paths must pass [`validate_paths`]. Returns the stored list. Blocking.
pub fn save_path_set(
    store: &ConfigStore,
    set: PathSet,
    replaces: Option<&str>,
) -> Result<Vec<PathSet>> {
    let name = set.name.trim().to_owned();
    let length = name.chars().count();
    if length == 0 {
        return Err(Error::Usage("the path set needs a name".into()));
    }
    if length > MAX_SET_NAME_CHARS {
        return Err(Error::Usage(format!(
            "the name is {length} characters long; the maximum is {MAX_SET_NAME_CHARS}"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(Error::Usage(
            "the name must not contain control characters".into(),
        ));
    }
    let set = PathSet {
        name,
        paths: validate_paths(&set.paths)?,
    };
    store.update(|config| {
        let sets = &mut config.invalidation_path_sets;
        let position = match replaces {
            Some(old) => Some(set_position(sets, old).ok_or_else(|| missing_set(old))?),
            None => None,
        };
        if let Some(other) = set_position(sets, &set.name)
            && Some(other) != position
        {
            return Err(Error::Conflict(format!(
                "a path set named '{}' already exists",
                sets[other].name
            )));
        }
        match position {
            Some(at) => sets[at] = set,
            None if sets.len() >= MAX_PATH_SETS => {
                return Err(Error::Conflict(format!(
                    "at most {MAX_PATH_SETS} path sets can be saved; delete one first"
                )));
            }
            None => sets.push(set),
        }
        Ok(sets.clone())
    })
}

/// Removes the set named `name` (ignoring case) and returns the stored list;
/// `not_found` when there is none. Blocking.
pub fn delete_path_set(store: &ConfigStore, name: &str) -> Result<Vec<PathSet>> {
    store.update(|config| {
        let at =
            set_position(&config.invalidation_path_sets, name).ok_or_else(|| missing_set(name))?;
        config.invalidation_path_sets.remove(at);
        Ok(config.invalidation_path_sets.clone())
    })
}

fn set_position(sets: &[PathSet], name: &str) -> Option<usize> {
    let name = name.trim().to_lowercase();
    sets.iter().position(|set| set.name.to_lowercase() == name)
}

fn missing_set(name: &str) -> Error {
    Error::NotFound(format!("there is no path set named '{}'", name.trim()))
}

#[cfg(test)]
mod tests;
