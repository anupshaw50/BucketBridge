//! Saved sync jobs (v0.2.0): named sync setups kept in `config.toml`.
//!
//! A [`SyncJob`] holds everything a sync needs: account, direction, the local
//! folder and S3 location (for `s3_to_s3` the source and destination prefixes),
//! the delete flag and the [`SyncOptions`] (filters, exclusion rules or file, and the
//! analysis options). Jobs are listed, saved and deleted here, and run by the desktop
//! `run_sync_job` command and `bb sync --job NAME`.
//!
//! Every change is a read-modify-write under the configuration lock
//! ([`ConfigStore::update`]). Names are trimmed, 1 to [`MAX_NAME_CHARS`] characters,
//! free of control characters and unique (compared without regard to letter case).
//! At most [`MAX_JOBS`] jobs can be stored; removing an account drops its jobs.
//!
//! All functions here block (they read and write the configuration file); async
//! callers use `spawn_blocking`.

use serde::{Deserialize, Serialize};

use crate::config::{ConfigStore, new_uuid_v4};
use crate::error::{Error, Result};
use crate::model::SyncDirection;
use crate::s3uri::S3Uri;
use crate::sync::SyncOptions;

/// Longest job name, in characters.
pub const MAX_NAME_CHARS: usize = 100;

/// Most jobs that can be stored.
pub const MAX_JOBS: usize = 200;

/// A saved sync setup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncJob {
    /// Random id, stable across renames; empty in a job that is about to be created.
    #[serde(default)]
    pub id: String,
    /// Name shown in the UI and used by `bb sync --job`.
    pub name: String,
    /// Account the job runs with.
    pub account_id: String,
    /// Which way files are copied.
    pub direction: SyncDirection,
    /// Local folder (an absolute path); empty for `s3_to_s3`.
    #[serde(default)]
    pub local_root: String,
    /// `s3://bucket/prefix/`; the destination for `s3_to_s3`.
    pub s3_prefix: String,
    /// Source `s3://bucket/prefix/` of an `s3_to_s3` job; `None` for every other direction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s3_source: Option<String>,
    /// Delete what exists only on the target side (not with `bidirectional`).
    #[serde(default)]
    pub delete: bool,
    /// Filters and analysis options.
    #[serde(default)]
    pub options: SyncOptions,
}

impl SyncJob {
    /// Checks the job without the configuration: name, locations against the
    /// direction, the delete flag and the options. Returns the job with the name
    /// trimmed and the prefixes in their folder form.
    pub fn validated(mut self) -> Result<SyncJob> {
        self.name = valid_name(self.name.trim())?;
        if self.account_id.trim().is_empty() {
            return Err(Error::Usage("a sync job needs an account".into()));
        }
        let destination = crate::sync::as_folder(&S3Uri::parse(self.s3_prefix.trim())?)?;
        self.s3_prefix = destination.to_string();
        match self.direction {
            SyncDirection::S3ToS3 => {
                let source = self.s3_source.take().ok_or_else(|| {
                    Error::Usage("an S3 to S3 sync job needs a source prefix".into())
                })?;
                let source = crate::sync::as_folder(&S3Uri::parse(source.trim())?)?;
                if source == destination {
                    return Err(Error::Usage(
                        "the source and the destination of the job are the same prefix".into(),
                    ));
                }
                if source.bucket() == destination.bucket()
                    && (source.key().starts_with(destination.key())
                        || destination.key().starts_with(source.key()))
                {
                    return Err(Error::Usage(
                        "the source and the destination must not contain each other".into(),
                    ));
                }
                self.s3_source = Some(source.to_string());
                self.local_root.clear();
            }
            _ => {
                self.s3_source = None;
                if self.local_root.trim().is_empty() {
                    return Err(Error::Usage("a sync job needs a local folder".into()));
                }
                self.local_root = self.local_root.trim().to_owned();
            }
        }
        if self.delete && self.direction == SyncDirection::Bidirectional {
            return Err(Error::Usage(
                "delete cannot be combined with bidirectional sync".into(),
            ));
        }
        self.options.validate()?;
        Ok(self)
    }
}

/// The stored jobs, in the order they were created.
pub fn list(store: &ConfigStore) -> Result<Vec<SyncJob>> {
    Ok(store.load()?.sync_jobs)
}

/// The job whose id or name (compared without regard to letter case) is `id_or_name`;
/// `not_found` when there is none.
pub fn find(store: &ConfigStore, id_or_name: &str) -> Result<SyncJob> {
    let jobs = list(store)?;
    lookup(&jobs, id_or_name)
        .cloned()
        .ok_or_else(|| missing(id_or_name))
}

fn lookup<'a>(jobs: &'a [SyncJob], id_or_name: &str) -> Option<&'a SyncJob> {
    jobs.iter().find(|job| job.id == id_or_name).or_else(|| {
        jobs.iter()
            .find(|job| job.name.to_lowercase() == id_or_name.trim().to_lowercase())
    })
}

/// Creates a job (empty `id`) or replaces the stored job with that id, and returns it
/// as stored. Fails with `usage` for an invalid job, `not_found` for an unknown
/// account or id, and `conflict` for a name that another job uses or when
/// [`MAX_JOBS`] would be exceeded.
pub fn save(store: &ConfigStore, job: SyncJob) -> Result<SyncJob> {
    let mut job = job.validated()?;
    if job.id.is_empty() {
        job.id = new_uuid_v4()?;
        return store.update(|config| {
            check_account(config, &job)?;
            check_name(&config.sync_jobs, &job)?;
            if config.sync_jobs.len() >= MAX_JOBS {
                return Err(Error::Conflict(format!(
                    "at most {MAX_JOBS} sync jobs can be saved; delete one first"
                )));
            }
            config.sync_jobs.push(job.clone());
            Ok(job.clone())
        });
    }
    store.update(|config| {
        check_account(config, &job)?;
        check_name(&config.sync_jobs, &job)?;
        let slot = config
            .sync_jobs
            .iter_mut()
            .find(|stored| stored.id == job.id)
            .ok_or_else(|| missing(&job.id))?;
        *slot = job.clone();
        Ok(job.clone())
    })
}

/// Removes the job `id` and returns the stored list; `not_found` for an unknown id.
pub fn delete(store: &ConfigStore, id: &str) -> Result<Vec<SyncJob>> {
    store.update(|config| {
        let at = config
            .sync_jobs
            .iter()
            .position(|job| job.id == id)
            .ok_or_else(|| missing(id))?;
        config.sync_jobs.remove(at);
        Ok(config.sync_jobs.clone())
    })
}

fn check_account(config: &crate::config::AppConfig, job: &SyncJob) -> Result<()> {
    if config
        .accounts
        .iter()
        .any(|account| account.id == job.account_id)
    {
        Ok(())
    } else {
        Err(Error::NotFound(format!(
            "there is no account '{}'",
            job.account_id
        )))
    }
}

fn check_name(jobs: &[SyncJob], job: &SyncJob) -> Result<()> {
    let lowered = job.name.to_lowercase();
    match jobs
        .iter()
        .find(|other| other.id != job.id && other.name.to_lowercase() == lowered)
    {
        Some(other) => Err(Error::Conflict(format!(
            "a sync job named '{}' already exists",
            other.name
        ))),
        None => Ok(()),
    }
}

fn valid_name(name: &str) -> Result<String> {
    let length = name.chars().count();
    if length == 0 {
        return Err(Error::Usage("the sync job needs a name".into()));
    }
    if length > MAX_NAME_CHARS {
        return Err(Error::Usage(format!(
            "the name is {length} characters long; the maximum is {MAX_NAME_CHARS}"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(Error::Usage(
            "the name must not contain control characters".into(),
        ));
    }
    Ok(name.to_owned())
}

fn missing(id: &str) -> Error {
    Error::NotFound(format!("there is no sync job '{id}'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::model::FilterSpec;

    fn store_with_account() -> (tempfile::TempDir, ConfigStore, String) {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::at(dir.path().join("config.toml"));
        let account_id = store
            .update(|config| {
                let id = "acct-1".to_owned();
                config.accounts.push(crate::model::Account {
                    id: id.clone(),
                    ..crate::model::Account::default()
                });
                Ok(id)
            })
            .unwrap();
        (dir, store, account_id)
    }

    fn job(name: &str, account_id: &str) -> SyncJob {
        SyncJob {
            id: String::new(),
            name: name.to_owned(),
            account_id: account_id.to_owned(),
            direction: SyncDirection::Upload,
            local_root: "/data/photos".to_owned(),
            s3_prefix: "s3://backup/photos".to_owned(),
            s3_source: None,
            delete: false,
            options: SyncOptions::default(),
        }
    }

    #[test]
    fn save_list_find_update_delete() {
        let (_dir, store, account) = store_with_account();
        let mut a = save(&store, job("  Photos  ", &account)).unwrap();
        assert_eq!(a.name, "Photos");
        assert_eq!(a.s3_prefix, "s3://backup/photos/");
        assert!(!a.id.is_empty());
        let b = save(&store, job("Docs", &account)).unwrap();
        assert_eq!(list(&store).unwrap().len(), 2);
        assert_eq!(find(&store, "photos").unwrap().id, a.id);
        assert_eq!(find(&store, &b.id).unwrap().name, "Docs");

        a.delete = true;
        a.options.exclude = vec!["*.tmp".into()];
        let updated = save(&store, a.clone()).unwrap();
        assert!(updated.delete);
        assert_eq!(find(&store, &a.id).unwrap().options.exclude, ["*.tmp"]);
        // Renaming keeps the id and frees the old name.
        a.name = "Pictures".into();
        save(&store, a.clone()).unwrap();
        assert_eq!(
            find(&store, "photos").unwrap_err().kind(),
            ErrorKind::NotFound
        );

        let left = delete(&store, &a.id).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(
            delete(&store, &a.id).unwrap_err().kind(),
            ErrorKind::NotFound
        );
    }

    #[test]
    fn names_are_unique_without_regard_to_case_and_validated() {
        let (_dir, store, account) = store_with_account();
        save(&store, job("Nightly", &account)).unwrap();
        let clash = save(&store, job("nightly", &account)).unwrap_err();
        assert_eq!(clash.kind(), ErrorKind::Conflict);
        for bad in ["", "   ", "a\nb", &"x".repeat(MAX_NAME_CHARS + 1)] {
            let err = save(&store, job(bad, &account)).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::Usage, "{bad:?}");
        }
        // Renaming a job to another job's name is a conflict too.
        let other = save(&store, job("Other", &account)).unwrap();
        let mut renamed = other;
        renamed.name = "NIGHTLY".into();
        assert_eq!(
            save(&store, renamed).unwrap_err().kind(),
            ErrorKind::Conflict
        );
    }

    #[test]
    fn jobs_are_checked_against_account_and_direction() {
        let (_dir, store, account) = store_with_account();
        let err = save(&store, job("x", "no-such-account")).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotFound);

        let mut both = job("both", &account);
        both.direction = SyncDirection::Bidirectional;
        both.delete = true;
        assert_eq!(save(&store, both).unwrap_err().kind(), ErrorKind::Usage);

        let mut no_folder = job("nofolder", &account);
        no_folder.local_root = " ".into();
        assert_eq!(
            save(&store, no_folder).unwrap_err().kind(),
            ErrorKind::Usage
        );

        let mut bad_filter = job("badfilter", &account);
        bad_filter.options.filter = FilterSpec {
            min_size: Some(5),
            max_size: Some(1),
            ..FilterSpec::default()
        };
        assert_eq!(
            save(&store, bad_filter).unwrap_err().kind(),
            ErrorKind::Usage
        );

        let mut copy = job("copy", &account);
        copy.direction = SyncDirection::S3ToS3;
        assert_eq!(
            save(&store, copy.clone()).unwrap_err().kind(),
            ErrorKind::Usage
        );
        copy.s3_source = Some("s3://backup/photos".into());
        assert_eq!(
            save(&store, copy.clone()).unwrap_err().kind(),
            ErrorKind::Usage
        );
        copy.s3_source = Some("s3://backup/photos/2024".into());
        assert_eq!(
            save(&store, copy.clone()).unwrap_err().kind(),
            ErrorKind::Usage
        );
        copy.s3_source = Some("s3://other/photos".into());
        let saved = save(&store, copy).unwrap();
        assert!(saved.local_root.is_empty());
        assert_eq!(saved.s3_source.as_deref(), Some("s3://other/photos/"));
    }

    #[test]
    fn the_job_limit_is_enforced() {
        let (_dir, store, account) = store_with_account();
        for number in 0..MAX_JOBS {
            save(&store, job(&format!("job {number}"), &account)).unwrap();
        }
        let err = save(&store, job("one too many", &account)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Conflict);
    }

    #[test]
    fn config_files_without_jobs_load_and_jobs_round_trip() {
        let (_dir, store, account) = store_with_account();
        assert!(list(&store).unwrap().is_empty());
        let text_before = std::fs::read_to_string(store.path()).unwrap();
        assert!(!text_before.contains("sync_jobs"));
        let mut with_options = job("full", &account);
        with_options.options.include = vec!["**/*.jpg".into()];
        with_options.options.exclude = vec!["thumbs/".into(), "!thumbs/keep.jpg".into()];
        with_options.options.exclude_file = Some("/etc/rules".into());
        with_options.options.skip_duplicate_content = true;
        with_options.options.parallelism = 8;
        let saved = save(&store, with_options).unwrap();
        let text = std::fs::read_to_string(store.path()).unwrap();
        assert!(text.contains("[[sync_jobs]]"), "{text}");
        assert_eq!(find(&store, "full").unwrap(), saved);
    }
}
