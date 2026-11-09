//! `bb sync jobs list|show|delete`: the saved sync jobs of `config.toml`.
//!
//! These commands only read and write the configuration, so they need no account,
//! no credentials and no S3 transport. Jobs are created in the desktop app or with
//! `bb sync --save-job NAME ...`, and run with `bb sync --job NAME`.

use bucketbridge_core::model::SyncDirection;
use bucketbridge_core::syncjobs::{self, SyncJob};
use bucketbridge_core::{ConfigStore, Error};
use serde::Serialize;

use crate::cli::{GlobalArgs, SyncJobsCommand};
use crate::context::{Inputs, dirs, env};
use crate::i18n::trf;
use crate::output::{Failure, Output};

/// The record of `bb sync jobs delete`.
#[derive(Debug, Serialize)]
struct DeletedRecord<'a> {
    id: &'a str,
    name: &'a str,
    deleted: bool,
}

/// Runs a `sync jobs` subcommand.
pub fn run(
    global: &GlobalArgs,
    command: &SyncJobsCommand,
    inputs: &Inputs,
    out: &mut Output,
) -> Result<(), Failure> {
    if global.account.is_some() || global.mfa_code.is_some() {
        return Err(Error::Usage(trf(
            "--account and --mfa-code do not apply to `bb sync jobs`; a job names its account",
            &[],
        ))
        .into());
    }
    let dirs = dirs(global, &inputs.env)?.ok_or_else(|| {
        Error::Usage(trf(
            "no configuration directory for the sync jobs: set {env} or pass --config-dir",
            &[("env", env::CONFIG_DIR)],
        ))
    })?;
    let store = ConfigStore::new(&dirs);
    match command {
        SyncJobsCommand::List => {
            for job in syncjobs::list(&store)? {
                let line = one_line(&job);
                out.row(&job, || line)?;
            }
            Ok(())
        }
        SyncJobsCommand::Show(selector) => {
            let job = syncjobs::find(&store, &selector.name)?;
            let text = details(&job);
            out.row(&job, || text)
        }
        SyncJobsCommand::Delete(selector) => {
            let job = syncjobs::find(&store, &selector.name)?;
            if !selector.yes {
                return Err(Error::Usage(trf(
                    "deleting the sync job '{name}' needs --yes",
                    &[("name", &job.name)],
                ))
                .into());
            }
            syncjobs::delete(&store, &job.id)?;
            let record = DeletedRecord {
                id: &job.id,
                name: &job.name,
                deleted: true,
            };
            out.change(&record, || {
                trf("deleted sync job {name}", &[("name", &job.name)])
            })
        }
    }
}

fn direction_name(direction: SyncDirection) -> &'static str {
    match direction {
        SyncDirection::Upload => "upload",
        SyncDirection::Download => "download",
        SyncDirection::Bidirectional => "bidirectional",
        SyncDirection::S3ToS3 => "s3_to_s3",
    }
}

/// The first location of a job: the local folder, or the source prefix.
fn from_side(job: &SyncJob) -> &str {
    job.s3_source.as_deref().unwrap_or(&job.local_root)
}

fn one_line(job: &SyncJob) -> String {
    format!(
        "{}\t{}\t{} -> {}",
        job.name,
        direction_name(job.direction),
        from_side(job),
        job.s3_prefix
    )
}

fn details(job: &SyncJob) -> String {
    let options = &job.options;
    let mut lines = vec![
        trf("name: {name}", &[("name", &job.name)]),
        trf("id: {id}", &[("id", &job.id)]),
        trf("account: {account}", &[("account", &job.account_id)]),
        trf(
            "direction: {direction}",
            &[("direction", direction_name(job.direction))],
        ),
        trf("from: {from}", &[("from", from_side(job))]),
        trf("to: {to}", &[("to", &job.s3_prefix)]),
        trf(
            "delete: {flag}",
            &[("flag", job.delete.to_string().as_str())],
        ),
    ];
    if !options.include.is_empty() {
        lines.push(trf(
            "include: {list}",
            &[("list", options.include.join(", ").as_str())],
        ));
    }
    if !options.exclude.is_empty() {
        lines.push(trf(
            "exclude: {list}",
            &[("list", options.exclude.join(", ").as_str())],
        ));
    }
    if let Some(file) = &options.exclude_file {
        lines.push(trf("exclude file: {file}", &[("file", file)]));
    }
    lines.push(trf(
        "skip empty folders: {flag}",
        &[("flag", options.skip_empty_folders.to_string().as_str())],
    ));
    lines.push(trf(
        "skip duplicate content: {flag}",
        &[("flag", options.skip_duplicate_content.to_string().as_str())],
    ));
    lines.push(trf(
        "use cache: {flag}",
        &[("flag", options.use_cache.to_string().as_str())],
    ));
    lines.push(trf(
        "parallelism: {count}",
        &[("count", options.parallelism.to_string().as_str())],
    ));
    lines.join("\n")
}
