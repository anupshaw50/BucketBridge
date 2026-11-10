//! `bb clone-bucket`: a new bucket with the settings of an existing one, and
//! its objects (server-side copies, as `bb cp -r`).

use bucketbridge_core::bucketclone::{
    ApplyState, ClonePreview, CloneRequest, CloneSetting, SettingState,
};
use bucketbridge_core::filter::IncludeExclude;
use bucketbridge_core::{Error, S3Uri};

use super::copy::{self, CopyAccounts, Mode};
use crate::cli::{CloneBucketArgs, CopyArgs, CseArgs};
use crate::context::Context;
use crate::i18n::{tr, trf};
use crate::location::Location;
use crate::output::{Failure, Output};
use crate::records::CloneSettingRecord;

/// The settings `--settings` names: `all` (the default), `none`, or a comma
/// separated list of setting names.
pub fn parse_settings(text: &str) -> Result<Vec<CloneSetting>, Error> {
    match text.trim() {
        "all" => Ok(CloneSetting::ALL.to_vec()),
        "none" => Ok(Vec::new()),
        list => list
            .split(',')
            .map(|name| name.trim().parse::<CloneSetting>())
            .collect(),
    }
}

/// Checks the arguments that need no server.
pub fn check(args: &CloneBucketArgs) -> Result<(), Error> {
    parse_settings(&args.settings)?;
    if args.source == args.new_bucket {
        return Err(Error::Usage(tr(
            "the new bucket must have a different name than the source",
        )));
    }
    if let Some(prefix) = &args.prefix
        && !prefix.is_empty()
        && !prefix.ends_with('/')
    {
        return Err(Error::Usage(trf(
            "the prefix '{prefix}' must end with '/'",
            &[("prefix", prefix.as_str())],
        )));
    }
    Ok(())
}

/// Runs `bb clone-bucket`.
pub async fn run(ctx: &Context, args: &CloneBucketArgs, out: &mut Output) -> Result<(), Failure> {
    check(args)?;
    let wanted = parse_settings(&args.settings)?;
    if args.dry_run {
        let preview = ctx.s3.preview_bucket_clone(&args.source).await?;
        for item in &preview.settings {
            let record = CloneSettingRecord {
                bucket: args.new_bucket.clone(),
                setting: item.setting.as_str().to_owned(),
                state: preview_state(item.state).to_owned(),
                count: item.count,
                detail: item.detail.clone(),
                dry_run: true,
                error: item.error.clone(),
            };
            out.change(&record, || preview_line(&preview, item.setting, &wanted))?;
        }
        return Ok(());
    }
    let outcome = ctx
        .s3
        .create_bucket_clone(&CloneRequest {
            source_bucket: args.source.clone(),
            new_bucket: args.new_bucket.clone(),
            region: args.region.clone(),
            settings: wanted,
        })
        .await?;
    out.change(
        &crate::records::BucketRecord {
            bucket: outcome.bucket.clone(),
            region: outcome.region.clone(),
            object_lock: None,
            ownership: None,
            public_access_block: None,
            warnings: Vec::new(),
        },
        || trf("created bucket {bucket}", &[("bucket", &outcome.bucket)]),
    )?;
    let mut failed = false;
    for result in &outcome.results {
        failed |= result.state == ApplyState::Failed;
        let record = CloneSettingRecord {
            bucket: outcome.bucket.clone(),
            setting: result.setting.as_str().to_owned(),
            state: apply_state(result.state).to_owned(),
            count: None,
            detail: None,
            dry_run: false,
            error: result.error.clone(),
        };
        out.change(&record, || result_line(&record))?;
    }
    if !args.no_objects {
        copy_objects(ctx, args, out).await?;
    }
    if failed {
        return Err(Error::Remote(tr(
            "some settings could not be copied; the bucket was created",
        ))
        .into());
    }
    Ok(())
}

/// The recursive server-side copy of the objects, with their keys unchanged.
async fn copy_objects(
    ctx: &Context,
    args: &CloneBucketArgs,
    out: &mut Output,
) -> Result<(), Failure> {
    let prefix = args.prefix.clone().unwrap_or_default();
    // A folder lands under the destination with its own name, so a prefix
    // goes to its parent to keep the keys.
    let parent = match prefix.trim_end_matches('/').rfind('/') {
        Some(at) => prefix[..=at].to_owned(),
        None => String::new(),
    };
    let source = S3Uri::new(&args.source, &prefix)?;
    let destination = S3Uri::new(
        &args.new_bucket,
        if prefix.is_empty() { "" } else { &parent },
    )?;
    let copy_args = CopyArgs {
        source: Location::S3(source),
        destination: Location::S3(destination),
        recursive: true,
        include: Vec::new(),
        exclude: Vec::new(),
        dry_run: false,
        if_exists: None,
        no_preserve_mtime: false,
        no_upload_rules: true,
        from_account: None,
        to_account: None,
        cse: CseArgs::default(),
    };
    copy::run(
        ctx,
        &copy_args,
        &IncludeExclude::default(),
        Mode::Copy,
        None,
        &CopyAccounts::default(),
        out,
    )
    .await
}

fn preview_state(state: SettingState) -> &'static str {
    match state {
        SettingState::Configured => "configured",
        SettingState::NotConfigured => "not_configured",
        SettingState::Unsupported => "unsupported",
        SettingState::Failed => "failed",
    }
}

fn apply_state(state: ApplyState) -> &'static str {
    match state {
        ApplyState::Applied => "applied",
        ApplyState::Skipped => "skipped",
        ApplyState::Failed => "failed",
    }
}

/// `setting: what the source has` (a dry run).
fn preview_line(preview: &ClonePreview, setting: CloneSetting, wanted: &[CloneSetting]) -> String {
    let Some(item) = preview.settings.iter().find(|item| item.setting == setting) else {
        return String::new();
    };
    let value = match item.state {
        SettingState::Configured => {
            if wanted.contains(&setting) {
                tr("would be copied")
            } else {
                tr("configured, not selected")
            }
        }
        SettingState::NotConfigured => tr("not set on the source"),
        SettingState::Unsupported => tr("not supported by the server"),
        SettingState::Failed => tr("could not be read"),
    };
    format!("{}: {value}", setting.as_str())
}

/// `setting: copied | nothing to copy | failed: reason`.
fn result_line(record: &CloneSettingRecord) -> String {
    let value = match record.state.as_str() {
        "applied" => tr("copied"),
        "skipped" => tr("nothing to copy"),
        _ => trf(
            "failed: {error}",
            &[(
                "error",
                record
                    .error
                    .as_ref()
                    .map_or("", |error| error.message.as_str()),
            )],
        ),
    };
    format!("{}: {value}", record.setting)
}
