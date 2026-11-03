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
        return Err(Error::Usage(
            "--account and --mfa-code do not apply to `bb sync jobs`; a job names its account"
                .into(),
        )
        .into());
    }
    let dirs = dirs(global, &inputs.env)?.ok_or_else(|| {
        Error::Usage(format!(
            "no configuration directory for the sync jobs: set {} or pass --config-dir",
            env::CONFIG_DIR
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
                return Err(Error::Usage(format!(
                    "deleting the sync job '{}' needs --yes",
                    job.name
                ))
                .into());
            }
            syncjobs::delete(&store, &job.id)?;
            let record = DeletedRecord {
                id: &job.id,
                name: &job.name,
                deleted: true,
            };
            out.change(&record, || format!("deleted sync job {}", job.name))
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
        format!("name: {}", job.name),
        format!("id: {}", job.id),
        format!("account: {}", job.account_id),
        format!("direction: {}", direction_name(job.direction)),
        format!("from: {}", from_side(job)),
        format!("to: {}", job.s3_prefix),
        format!("delete: {}", job.delete),
    ];
    if !options.include.is_empty() {
        lines.push(format!("include: {}", options.include.join(", ")));
    }
    if !options.exclude.is_empty() {
        lines.push(format!("exclude: {}", options.exclude.join(", ")));
    }
    if let Some(file) = &options.exclude_file {
        lines.push(format!("exclude file: {file}"));
    }
    lines.push(format!(
        "skip empty folders: {}",
        options.skip_empty_folders
    ));
    lines.push(format!(
        "skip duplicate content: {}",
        options.skip_duplicate_content
    ));
    lines.push(format!("use cache: {}", options.use_cache));
    lines.push(format!("parallelism: {}", options.parallelism));
    lines.join("\n")
}
