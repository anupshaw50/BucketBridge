//! `bb cloudfront`: distributions and invalidations of the selected AWS
//! account (the core module `bucketbridge_core::cloudfront`).
//!
//! Records are the core types as they are (see docs/cli.md); a distribution
//! changed by `enable`, `disable` and `detach` is printed as read back.

use bucketbridge_core::Error;
use bucketbridge_core::cloudfront::{Distribution, Invalidation, NewDistribution};
use serde::Serialize;

use super::timestamp;
use crate::cli::{
    CloudfrontCommand, CloudfrontCreateArgs, CloudfrontDetachArgs, CloudfrontIdArgs,
    CloudfrontInvalidateArgs, CloudfrontInvalidationArgs, CloudfrontListArgs,
};
use crate::context::Context;
use crate::i18n::{tr, trf};
use crate::output::{Failure, Output};

/// Runs one `bb cloudfront` subcommand.
pub async fn run(
    ctx: &Context,
    command: &CloudfrontCommand,
    out: &mut Output,
) -> Result<(), Failure> {
    match command {
        CloudfrontCommand::List(args) => list(ctx, args, out).await,
        CloudfrontCommand::Show(args) => show(ctx, args, out).await,
        CloudfrontCommand::Create(args) => create(ctx, args, out).await,
        CloudfrontCommand::Enable(args) => set_enabled(ctx, args, true, out).await,
        CloudfrontCommand::Disable(args) => set_enabled(ctx, args, false, out).await,
        CloudfrontCommand::Detach(args) => detach(ctx, args, out).await,
        CloudfrontCommand::Invalidate(args) => invalidate(ctx, args, out).await,
        CloudfrontCommand::Invalidations(args) => invalidations(ctx, args, out).await,
        CloudfrontCommand::Invalidation(args) => invalidation(ctx, args, out).await,
    }
}

fn state(enabled: bool) -> &'static str {
    if enabled { "enabled" } else { "disabled" }
}

fn distribution_line(distribution: &Distribution) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}",
        distribution.id,
        distribution.status,
        state(distribution.enabled),
        distribution.domain_name,
        distribution.comment
    )
}

fn invalidation_line(invalidation: &Invalidation) -> String {
    let mut line = format!(
        "{}\t{}\t{}",
        invalidation.id,
        invalidation.status,
        timestamp(invalidation.create_time)
    );
    if !invalidation.paths.is_empty() {
        line.push('\t');
        line.push_str(&invalidation.paths.join(" "));
    }
    line
}

async fn list(ctx: &Context, args: &CloudfrontListArgs, out: &mut Output) -> Result<(), Failure> {
    let cloudfront = ctx.cloudfront().await?;
    let distributions = match &args.bucket {
        Some(bucket) => cloudfront.distributions_for_bucket(bucket).await?,
        None => cloudfront.list_all_distributions().await?,
    };
    for distribution in &distributions {
        out.row(distribution, || distribution_line(distribution))?;
    }
    Ok(())
}

fn details(distribution: &Distribution) -> String {
    let mut lines = vec![
        trf("id: {id}", &[("id", distribution.id.as_str())]),
        trf("arn: {arn}", &[("arn", distribution.arn.as_str())]),
        trf(
            "domain: {domain}",
            &[("domain", distribution.domain_name.as_str())],
        ),
        trf(
            "status: {status}",
            &[("status", distribution.status.as_str())],
        ),
        trf("state: {state}", &[("state", state(distribution.enabled))]),
        trf(
            "comment: {comment}",
            &[("comment", distribution.comment.as_str())],
        ),
        trf(
            "price class: {price_class}",
            &[("price_class", distribution.price_class.as_str())],
        ),
        trf(
            "last modified: {time}",
            &[("time", timestamp(distribution.last_modified).as_str())],
        ),
    ];
    if !distribution.aliases.is_empty() {
        lines.push(trf(
            "aliases: {aliases}",
            &[("aliases", distribution.aliases.join(", ").as_str())],
        ));
    }
    if let Some(root) = distribution
        .default_root_object
        .as_deref()
        .filter(|root| !root.is_empty())
    {
        lines.push(trf("default root object: {root}", &[("root", root)]));
    }
    if let Some(count) = distribution.in_progress_invalidations {
        lines.push(trf(
            "invalidations in progress: {count}",
            &[("count", count.to_string().as_str())],
        ));
    }
    for origin in &distribution.origins {
        let mut line = trf(
            "origin {id}: {domain}",
            &[
                ("id", origin.id.as_str()),
                ("domain", origin.domain_name.as_str()),
            ],
        );
        if !origin.origin_path.is_empty() {
            line.push_str(&origin.origin_path);
        }
        if let Some(bucket) = &origin.bucket {
            line.push_str(&trf(" (bucket {bucket}", &[("bucket", bucket.as_str())]));
            if origin.website {
                line.push_str(&tr(", website endpoint"));
            }
            if origin.access_control {
                line.push_str(&tr(", origin access control"));
            }
            line.push_str(&tr(")"));
        }
        lines.push(line);
    }
    lines.join("\n")
}

async fn show(ctx: &Context, args: &CloudfrontIdArgs, out: &mut Output) -> Result<(), Failure> {
    let distribution = ctx.cloudfront().await?.get_distribution(&args.id).await?;
    out.row(&distribution, || details(&distribution))
}

/// `bb cloudfront create`
#[derive(Serialize)]
struct CreateRecord<'a> {
    distribution: &'a Distribution,
    origin_access_control_id: &'a str,
    bucket_policy: &'a str,
}

async fn create(
    ctx: &Context,
    args: &CloudfrontCreateArgs,
    out: &mut Output,
) -> Result<(), Failure> {
    let cloudfront = ctx.cloudfront().await?;
    let region = match &args.region {
        Some(region) => region.clone(),
        None => ctx
            .s3
            .list_buckets()
            .await?
            .into_iter()
            .find(|bucket| bucket.name == args.bucket)
            .and_then(|bucket| bucket.region)
            .ok_or_else(|| {
                Error::Usage(trf(
                    "the region of bucket {bucket} is unknown: pass --region",
                    &[("bucket", &args.bucket)],
                ))
            })?,
    };
    let new = NewDistribution {
        bucket: args.bucket.clone(),
        region,
        comment: args.comment.clone(),
        enabled: !args.disabled,
        default_root_object: args.default_root_object.clone(),
        price_class: args.price_class.clone(),
    };
    let created = cloudfront.create_distribution(&new).await?;
    let record = CreateRecord {
        distribution: &created.distribution,
        origin_access_control_id: &created.origin_access_control_id,
        bucket_policy: &created.bucket_policy,
    };
    out.change(&record, || {
        trf(
            "created distribution {id} ({domain}) for bucket {bucket}\nThe bucket is not changed. Add this statement to the bucket policy of {policy_bucket} so that the distribution can read it:\n{policy}",
            &[
                ("id", created.distribution.id.as_str()),
                ("domain", created.distribution.domain_name.as_str()),
                ("bucket", &args.bucket),
                ("policy_bucket", &args.bucket),
                ("policy", &created.bucket_policy),
            ],
        )
    })
}

async fn set_enabled(
    ctx: &Context,
    args: &CloudfrontIdArgs,
    enabled: bool,
    out: &mut Output,
) -> Result<(), Failure> {
    let distribution = ctx
        .cloudfront()
        .await?
        .set_enabled(&args.id, enabled, None)
        .await?;
    out.change(&distribution, || {
        trf(
            if enabled {
                "enabled distribution {id}"
            } else {
                "disabled distribution {id}"
            },
            &[("id", distribution.id.as_str())],
        )
    })
}

async fn detach(
    ctx: &Context,
    args: &CloudfrontDetachArgs,
    out: &mut Output,
) -> Result<(), Failure> {
    let distribution = ctx
        .cloudfront()
        .await?
        .detach_bucket(&args.id, &args.bucket, None)
        .await?;
    out.change(&distribution, || {
        trf(
            "distribution {id} no longer serves bucket {bucket}",
            &[("id", distribution.id.as_str()), ("bucket", &args.bucket)],
        )
    })
}

async fn invalidate(
    ctx: &Context,
    args: &CloudfrontInvalidateArgs,
    out: &mut Output,
) -> Result<(), Failure> {
    let invalidation = ctx
        .cloudfront()
        .await?
        .create_invalidation(&args.id, &args.paths)
        .await?;
    out.change(&invalidation, || {
        trf(
            "created invalidation {id} ({status}): {paths}",
            &[
                ("id", invalidation.id.as_str()),
                ("status", invalidation.status.as_str()),
                ("paths", invalidation.paths.join(" ").as_str()),
            ],
        )
    })
}

async fn invalidations(
    ctx: &Context,
    args: &CloudfrontIdArgs,
    out: &mut Output,
) -> Result<(), Failure> {
    let cloudfront = ctx.cloudfront().await?;
    let mut marker: Option<String> = None;
    loop {
        let page = cloudfront
            .list_invalidations(&args.id, marker.as_deref())
            .await?;
        for invalidation in &page.items {
            out.row(invalidation, || invalidation_line(invalidation))?;
        }
        match page.next_marker {
            Some(next) => marker = Some(next),
            None => return Ok(()),
        }
    }
}

async fn invalidation(
    ctx: &Context,
    args: &CloudfrontInvalidationArgs,
    out: &mut Output,
) -> Result<(), Failure> {
    let invalidation = ctx
        .cloudfront()
        .await?
        .get_invalidation(&args.id, &args.invalidation_id)
        .await?;
    out.row(&invalidation, || invalidation_line(&invalidation))
}
