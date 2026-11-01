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
        format!("id: {}", distribution.id),
        format!("arn: {}", distribution.arn),
        format!("domain: {}", distribution.domain_name),
        format!("status: {}", distribution.status),
        format!("state: {}", state(distribution.enabled)),
        format!("comment: {}", distribution.comment),
        format!("price class: {}", distribution.price_class),
        format!("last modified: {}", timestamp(distribution.last_modified)),
    ];
    if !distribution.aliases.is_empty() {
        lines.push(format!("aliases: {}", distribution.aliases.join(", ")));
    }
    if let Some(root) = distribution
        .default_root_object
        .as_deref()
        .filter(|root| !root.is_empty())
    {
        lines.push(format!("default root object: {root}"));
    }
    if let Some(count) = distribution.in_progress_invalidations {
        lines.push(format!("invalidations in progress: {count}"));
    }
    for origin in &distribution.origins {
        let mut line = format!("origin {}: {}", origin.id, origin.domain_name);
        if !origin.origin_path.is_empty() {
            line.push_str(&origin.origin_path);
        }
        if let Some(bucket) = &origin.bucket {
            line.push_str(&format!(" (bucket {bucket}"));
            if origin.website {
                line.push_str(", website endpoint");
            }
            if origin.access_control {
                line.push_str(", origin access control");
            }
            line.push(')');
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
                Error::Usage(format!(
                    "the region of bucket {} is unknown: pass --region",
                    args.bucket
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
        format!(
            "created distribution {} ({}) for bucket {}\nThe bucket is not changed. Add this statement to the bucket policy of {} so that the distribution can read it:\n{}",
            created.distribution.id,
            created.distribution.domain_name,
            args.bucket,
            args.bucket,
            created.bucket_policy
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
        let verb = if enabled { "enabled" } else { "disabled" };
        format!("{verb} distribution {}", distribution.id)
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
        format!(
            "distribution {} no longer serves bucket {}",
            distribution.id, args.bucket
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
        format!(
            "created invalidation {} ({}): {}",
            invalidation.id,
            invalidation.status,
            invalidation.paths.join(" ")
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
