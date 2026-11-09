//! Account folders, the order of accounts, and the account export file.
//!
//! # Folders and order
//!
//! `config.toml` keeps the accounts in a list; that list is also their order
//! in the account manager and the account switcher. Folders are a separate
//! list of named groups, each naming the accounts in it (`account_folders`),
//! so an account itself carries no folder field and an older configuration
//! without folders loads unchanged: every account is simply not in a folder.
//! Accounts that are in no folder are shown after the folders, in list order.
//! [`AccountLayout`] is the whole arrangement; [`AccountManager::set_layout`]
//! replaces it in one step (that is also how a folder is created, renamed and
//! deleted).
//!
//! # The export file
//!
//! [`AccountExport`] is a versioned JSON document (`format`
//! `bucketbridge-accounts`, `version` 1) with the folders and the accounts
//! in their order. It never holds a secret: no secret access key, session
//! token, proxy password or secret reference. An account with static keys is
//! exported with its access key ID and imported as an account that **needs a
//! secret** (its `secret_ref` is empty; the account cannot be used until the
//! secret access key is entered). Accounts that assume a role name their
//! source account by label, because ids are local to an installation.
//!
//! # Import
//!
//! [`AccountManager::import_accounts`] never overwrites: an account whose
//! label is taken (case-insensitively), or that is the same account as an
//! existing one (same access key ID, endpoint and region; or the same
//! non-key source), is a duplicate. [`DuplicatePolicy::Skip`] leaves
//! duplicates out and reports them; [`DuplicatePolicy::Rename`] imports a
//! label duplicate under a free label (`Label (2)`) and still skips an
//! account that is the same as an existing one. Everything is saved in one
//! configuration update, or nothing is.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::accounts::{
    AccountManager, check_access_key, check_label, check_label_free, check_source,
};
use crate::client::{validate_endpoint, validate_region};
use crate::config::{AppConfig, new_uuid_v4};
use crate::error::{Error, IoContext, Result};
use crate::mfa::validate_mfa_serial;
use crate::model::{
    Account, AccountList, CredentialSource, MAX_ROLE_DURATION_SECS, MIN_ROLE_DURATION_SECS,
};

/// `format` of an export file.
pub const EXPORT_FORMAT: &str = "bucketbridge-accounts";
/// Newest export file version this build reads and the one it writes.
pub const EXPORT_VERSION: u32 = 1;
/// Longest folder name in characters.
pub const MAX_FOLDER_NAME_CHARS: usize = 100;
/// Most accounts an import file may hold.
pub const MAX_IMPORT_ACCOUNTS: usize = 1000;
/// Largest import file accepted, in bytes.
const MAX_IMPORT_BYTES: u64 = 8 * 1024 * 1024;

/// A named group of accounts.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AccountFolder {
    /// Folder name, unique case-insensitively.
    pub name: String,
    /// Ids of the accounts in the folder, in their order.
    #[serde(default)]
    pub account_ids: Vec<String>,
}

/// The arrangement of accounts: what [`AccountManager::set_layout`] stores.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AccountLayout {
    /// The folders in their order, each with its accounts in order.
    pub folders: Vec<AccountFolder>,
    /// Every account id, in the order the accounts are listed. It must name
    /// exactly the stored accounts; accounts in no folder appear in this
    /// order after the folders.
    pub accounts: Vec<String>,
}

/// Where the credentials of an exported account come from. The same
/// choices as [`CredentialSource`], without anything that is local to this
/// installation (an assumed role names its source account by label).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExportedSource {
    /// Access keys; the secret is not exported.
    StaticKeys,
    /// A profile of the AWS shared config files.
    AwsProfile {
        /// Profile name.
        profile: String,
    },
    /// STS `AssumeRole` through another account.
    AssumeRole {
        /// Label of the account whose credentials call STS.
        source_account: String,
        /// ARN of the role.
        role_arn: String,
        /// Role session name.
        session_name: String,
        /// Session length in seconds.
        duration_secs: u32,
        /// External id of the trust policy.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        external_id: Option<String>,
        /// MFA device of the trust policy.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mfa_serial: Option<String>,
    },
    /// A role of an AWS account through IAM Identity Center.
    Sso {
        /// Start URL of the portal.
        start_url: String,
        /// Region of IAM Identity Center.
        sso_region: String,
        /// AWS account id.
        account_id: String,
        /// Permission set name.
        role_name: String,
        /// Name of the `sso-session`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sso_session: Option<String>,
    },
    /// The role of the instance or container BucketBridge runs on.
    InstanceRole,
}

/// One account in an export file. It has no secret of any kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedAccount {
    /// Display label.
    pub label: String,
    /// Name of the folder the account is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// Access key ID (not a secret); empty for sources without keys.
    #[serde(default)]
    pub access_key_id: String,
    /// Signing region.
    pub region: String,
    /// Custom endpoint URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Path-style addressing.
    #[serde(default)]
    pub path_style: bool,
    /// Send the requester-pays header.
    #[serde(default)]
    pub request_payer: bool,
    /// MFA device serial number or ARN.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mfa_serial: Option<String>,
    /// Where the credentials come from.
    pub source: ExportedSource,
}

/// The export file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountExport {
    /// Always [`EXPORT_FORMAT`].
    pub format: String,
    /// File version; see [`EXPORT_VERSION`].
    pub version: u32,
    /// When the file was written.
    pub exported_at: DateTime<Utc>,
    /// Folder names in their order (also folders without exported accounts).
    #[serde(default)]
    pub folders: Vec<String>,
    /// The accounts in their order.
    pub accounts: Vec<ExportedAccount>,
}

/// What to do with an imported account that is a duplicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DuplicatePolicy {
    /// Leave it out and report it.
    #[default]
    Skip,
    /// Import a label duplicate under a free label (`Label (2)`); an account
    /// that is the same as an existing one is still left out.
    Rename,
}

/// Why an imported account is a duplicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateReason {
    /// An account has the same label.
    Label,
    /// An account has the same access key ID, endpoint and region (or the
    /// same non-key source).
    SameAccount,
}

/// An account that was imported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedAccount {
    /// Id of the new account.
    pub id: String,
    /// Its label (after a rename).
    pub label: String,
    /// The label in the file when it was renamed.
    pub renamed_from: Option<String>,
    /// The account has static keys and no secret yet: enter its secret
    /// access key before using it.
    pub needs_secret: bool,
}

/// An account that was left out because it is a duplicate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportDuplicate {
    /// Label in the file.
    pub label: String,
    /// Label of the existing account it duplicates.
    pub existing: String,
    /// Why it counts as a duplicate.
    pub reason: DuplicateReason,
}

/// An account that was left out because it cannot be imported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportSkipped {
    /// Label in the file.
    pub label: String,
    /// Why.
    pub reason: String,
}

/// The outcome of an import.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportReport {
    /// Accounts that were added.
    pub imported: Vec<ImportedAccount>,
    /// Duplicates that were left out.
    pub duplicates: Vec<ImportDuplicate>,
    /// Accounts left out because they are invalid or depend on an account
    /// that is missing.
    pub skipped: Vec<ImportSkipped>,
    /// Folders that were created.
    pub folders_created: Vec<String>,
}

/// Checks a folder name: 1 to [`MAX_FOLDER_NAME_CHARS`] characters, no
/// control characters. Returns it trimmed.
pub fn check_folder_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::Usage("the folder name is empty".into()));
    }
    if name.chars().count() > MAX_FOLDER_NAME_CHARS {
        return Err(Error::Usage(format!(
            "the folder name must be at most {MAX_FOLDER_NAME_CHARS} characters"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(Error::Usage(
            "the folder name must not contain control characters".into(),
        ));
    }
    Ok(name)
}

/// The arrangement the configuration holds now: folders with their live
/// accounts, and every account id in list order.
pub fn layout_of(config: &AppConfig) -> AccountLayout {
    let live: BTreeSet<&str> = config.accounts.iter().map(|a| a.id.as_str()).collect();
    let mut seen = BTreeSet::new();
    let folders = config
        .account_folders
        .iter()
        .map(|folder| AccountFolder {
            name: folder.name.clone(),
            account_ids: folder
                .account_ids
                .iter()
                .filter(|id| live.contains(id.as_str()) && seen.insert((*id).clone()))
                .cloned()
                .collect(),
        })
        .collect();
    AccountLayout {
        folders,
        accounts: config.accounts.iter().map(|a| a.id.clone()).collect(),
    }
}

/// Checks `layout` against the stored accounts and stores it. Fails with
/// `conflict` when it does not name exactly the stored accounts (they changed
/// since the layout was made) and with `usage` for a bad folder.
pub fn apply_layout(config: &mut AppConfig, layout: &AccountLayout) -> Result<()> {
    let stored: BTreeSet<&str> = config.accounts.iter().map(|a| a.id.as_str()).collect();
    let given: BTreeSet<&str> = layout.accounts.iter().map(String::as_str).collect();
    if given.len() != layout.accounts.len() || given != stored {
        return Err(Error::Conflict(
            "the accounts changed since the list was loaded: reload it and try again".into(),
        ));
    }
    let mut names = BTreeSet::new();
    let mut placed = BTreeSet::new();
    let mut folders = Vec::new();
    for folder in &layout.folders {
        let name = check_folder_name(&folder.name)?;
        if !names.insert(name.to_lowercase()) {
            return Err(Error::Conflict(format!(
                "a folder named {name:?} already exists"
            )));
        }
        let mut ids = Vec::new();
        for id in &folder.account_ids {
            if !stored.contains(id.as_str()) {
                return Err(Error::Conflict(format!(
                    "folder {name:?} names an account that does not exist"
                )));
            }
            if !placed.insert(id.as_str()) {
                return Err(Error::Usage("an account can be in one folder only".into()));
            }
            ids.push(id.clone());
        }
        folders.push(AccountFolder {
            name: name.to_owned(),
            account_ids: ids,
        });
    }
    let position: BTreeMap<&str, usize> = layout
        .accounts
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect();
    let mut accounts = std::mem::take(&mut config.accounts);
    accounts.sort_by_key(|account| position.get(account.id.as_str()).copied());
    config.accounts = accounts;
    config.account_folders = folders;
    Ok(())
}

/// Removes an account from every folder (it was deleted).
pub(crate) fn forget_account(config: &mut AppConfig, id: &str) {
    for folder in &mut config.account_folders {
        folder.account_ids.retain(|member| member != id);
    }
}

/// Everything about the arrangement for the account list.
pub(crate) fn list_of(config: AppConfig) -> AccountList {
    let layout = layout_of(&config);
    AccountList {
        accounts: config.accounts,
        default_account_id: config.default_account_id,
        folders: layout.folders,
    }
}

fn exported_source(config: &AppConfig, account: &Account) -> Result<ExportedSource> {
    Ok(match &account.source {
        CredentialSource::StaticKeys => ExportedSource::StaticKeys,
        CredentialSource::AwsProfile { profile } => ExportedSource::AwsProfile {
            profile: profile.clone(),
        },
        CredentialSource::AssumeRole {
            source_account_id,
            role_arn,
            session_name,
            duration_secs,
            external_id,
            mfa_serial,
        } => {
            let source = config
                .accounts
                .iter()
                .find(|other| other.id == *source_account_id)
                .ok_or_else(|| {
                    Error::NotFound(format!(
                        "the source account of {:?} does not exist",
                        account.label
                    ))
                })?;
            ExportedSource::AssumeRole {
                source_account: source.label.clone(),
                role_arn: role_arn.clone(),
                session_name: session_name.clone(),
                duration_secs: *duration_secs,
                external_id: external_id.clone(),
                mfa_serial: mfa_serial.clone(),
            }
        }
        CredentialSource::Sso {
            start_url,
            sso_region,
            account_id,
            role_name,
            sso_session,
        } => ExportedSource::Sso {
            start_url: start_url.clone(),
            sso_region: sso_region.clone(),
            account_id: account_id.clone(),
            role_name: role_name.clone(),
            sso_session: sso_session.clone(),
        },
        CredentialSource::InstanceRole => ExportedSource::InstanceRole,
    })
}

/// Builds the export of the named accounts (all of them for `None`) in
/// their listed order. An account that assumes a role pulls its source
/// account into the file, so that the file imports on its own.
pub fn build_export(config: &AppConfig, ids: Option<&[String]>) -> Result<AccountExport> {
    let mut wanted: BTreeSet<String> = match ids {
        None => config.accounts.iter().map(|a| a.id.clone()).collect(),
        Some(ids) => {
            for id in ids {
                if !config.accounts.iter().any(|a| a.id == *id) {
                    return Err(Error::NotFound(format!("there is no account {id:?}")));
                }
            }
            ids.iter().cloned().collect()
        }
    };
    loop {
        let mut more = Vec::new();
        for account in config.accounts.iter().filter(|a| wanted.contains(&a.id)) {
            if let CredentialSource::AssumeRole {
                source_account_id, ..
            } = &account.source
                && !wanted.contains(source_account_id)
            {
                more.push(source_account_id.clone());
            }
        }
        if more.is_empty() {
            break;
        }
        wanted.extend(more);
    }
    let layout = layout_of(config);
    let folder_of: BTreeMap<&str, &str> = layout
        .folders
        .iter()
        .flat_map(|folder| {
            folder
                .account_ids
                .iter()
                .map(|id| (id.as_str(), folder.name.as_str()))
        })
        .collect();
    let mut accounts = Vec::new();
    for account in config.accounts.iter().filter(|a| wanted.contains(&a.id)) {
        accounts.push(ExportedAccount {
            label: account.label.clone(),
            folder: folder_of.get(account.id.as_str()).map(|n| (*n).to_owned()),
            access_key_id: account.access_key_id.clone(),
            region: account.region.clone(),
            endpoint: account.endpoint.clone(),
            path_style: account.path_style,
            request_payer: account.request_payer,
            mfa_serial: account.mfa_serial.clone(),
            source: exported_source(config, account)?,
        });
    }
    let used: BTreeSet<&str> = accounts
        .iter()
        .filter_map(|a| a.folder.as_deref())
        .collect();
    let folders = layout
        .folders
        .iter()
        .filter(|folder| ids.is_none() || used.contains(folder.name.as_str()))
        .map(|folder| folder.name.clone())
        .collect();
    Ok(AccountExport {
        format: EXPORT_FORMAT.to_owned(),
        version: EXPORT_VERSION,
        exported_at: Utc::now(),
        folders,
        accounts,
    })
}

/// Writes an export file atomically (temporary file, then rename). The file
/// holds no secret, but is still created readable by the current user only.
pub fn write_export(export: &AccountExport, path: &Path) -> Result<()> {
    let text = serde_json::to_string_pretty(export)
        .map_err(|err| Error::Internal(format!("serializing the account export: {err}")))?;
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let dir = dir.unwrap_or_else(|| Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(dir)
        .io_context(|| format!("creating a file next to {}", path.display()))?;
    temp.write_all(text.as_bytes())
        .and_then(|()| temp.write_all(b"\n"))
        .and_then(|()| temp.as_file().sync_all())
        .io_context(|| format!("writing {}", path.display()))?;
    temp.persist(path)
        .map_err(|err| err.error)
        .io_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// Reads and checks an export file: size, JSON, `format` and `version`.
pub fn read_export(path: &Path) -> Result<AccountExport> {
    let meta = fs::metadata(path).io_context(|| format!("reading {}", path.display()))?;
    if meta.len() > MAX_IMPORT_BYTES {
        return Err(Error::Usage(format!(
            "{} is too large to be an account file",
            path.display()
        )));
    }
    let text = fs::read_to_string(path).io_context(|| format!("reading {}", path.display()))?;
    parse_export(&text)
}

/// Parses and checks the text of an export file.
pub fn parse_export(text: &str) -> Result<AccountExport> {
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|err| Error::Usage(format!("this is not a BucketBridge account file: {err}")))?;
    if value.get("format").and_then(|f| f.as_str()) != Some(EXPORT_FORMAT) {
        return Err(Error::Usage(
            "this is not a BucketBridge account file".into(),
        ));
    }
    let version = value.get("version").and_then(serde_json::Value::as_u64);
    match version {
        Some(v) if v >= 1 && v <= u64::from(EXPORT_VERSION) => {}
        Some(v) => {
            return Err(Error::Usage(format!(
                "this account file has version {v}, which this version of BucketBridge cannot read"
            )));
        }
        None => {
            return Err(Error::Usage(
                "the account file has no version number".into(),
            ));
        }
    }
    let export: AccountExport = serde_json::from_value(value)
        .map_err(|err| Error::Usage(format!("the account file is damaged: {err}")))?;
    if export.accounts.len() > MAX_IMPORT_ACCOUNTS {
        return Err(Error::Usage(format!(
            "the account file holds more than {MAX_IMPORT_ACCOUNTS} accounts"
        )));
    }
    Ok(export)
}

/// Why an account in the file is not valid, or `None`.
fn check_exported(account: &ExportedAccount) -> Result<()> {
    check_label(&account.label)?;
    if let Some(folder) = &account.folder {
        check_folder_name(folder)?;
    }
    validate_region(&account.region)?;
    if let Some(endpoint) = account.endpoint.as_deref().filter(|e| !e.is_empty()) {
        validate_endpoint(endpoint)?;
    }
    if let Some(serial) = account.mfa_serial.as_deref().filter(|s| !s.is_empty()) {
        validate_mfa_serial(serial)?;
    }
    match &account.source {
        ExportedSource::StaticKeys => check_access_key(&account.access_key_id)?,
        ExportedSource::AssumeRole { duration_secs, .. }
            if !(MIN_ROLE_DURATION_SECS..=MAX_ROLE_DURATION_SECS).contains(duration_secs) =>
        {
            return Err(Error::Usage(format!(
                "the session length must be {MIN_ROLE_DURATION_SECS} to {MAX_ROLE_DURATION_SECS} seconds"
            )));
        }
        _ => {}
    }
    Ok(())
}

fn same_account(existing: &Account, incoming: &ExportedAccount) -> bool {
    match (&existing.source, &incoming.source) {
        (CredentialSource::StaticKeys, ExportedSource::StaticKeys) => {
            existing.access_key_id == incoming.access_key_id.trim()
                && existing.endpoint.as_deref().unwrap_or("")
                    == incoming.endpoint.as_deref().unwrap_or("")
                && existing.region == incoming.region
        }
        (
            CredentialSource::AwsProfile { profile: a },
            ExportedSource::AwsProfile { profile: b },
        ) => a == b && existing.region == incoming.region,
        (
            CredentialSource::Sso {
                start_url: a,
                account_id: b,
                role_name: c,
                ..
            },
            ExportedSource::Sso {
                start_url: x,
                account_id: y,
                role_name: z,
                ..
            },
        ) => a == x && b == y && c == z,
        (CredentialSource::InstanceRole, ExportedSource::InstanceRole) => {
            existing.region == incoming.region && existing.endpoint == incoming.endpoint
        }
        (
            CredentialSource::AssumeRole { role_arn: a, .. },
            ExportedSource::AssumeRole { role_arn: b, .. },
        ) => a == b,
        _ => false,
    }
}

/// A label that is free in `config`: `Label (2)`, `Label (3)`, ...
fn free_label(config: &AppConfig, label: &str) -> Result<String> {
    for n in 2..10_000 {
        let candidate = format!("{label} ({n})");
        if candidate.chars().count() <= 200 && check_label_free(config, &candidate, None).is_ok() {
            return Ok(candidate);
        }
    }
    Err(Error::Conflict(format!("no free label for {label:?}")))
}

/// Adds the accounts of `export` to `config` (see the [module docs](self))
/// and returns what happened. Accounts that need no other account come
/// first, so that an assumed role finds its source account in the file.
pub fn apply_import(
    config: &mut AppConfig,
    export: &AccountExport,
    policy: DuplicatePolicy,
) -> Result<ImportReport> {
    let mut report = ImportReport::default();
    // Label in the file -> id of the account it stands for (new or existing).
    let mut by_label: BTreeMap<String, String> = BTreeMap::new();
    let mut folder_members: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut order: Vec<&ExportedAccount> = export
        .accounts
        .iter()
        .filter(|a| !matches!(a.source, ExportedSource::AssumeRole { .. }))
        .collect();
    order.extend(
        export
            .accounts
            .iter()
            .filter(|a| matches!(a.source, ExportedSource::AssumeRole { .. })),
    );
    for incoming in order {
        if let Err(error) = check_exported(incoming) {
            report.skipped.push(ImportSkipped {
                label: incoming.label.clone(),
                reason: error.to_string(),
            });
            continue;
        }
        let label = incoming.label.trim();
        if let Some(existing) = config
            .accounts
            .iter()
            .find(|existing| same_account(existing, incoming))
        {
            by_label.insert(label.to_lowercase(), existing.id.clone());
            report.duplicates.push(ImportDuplicate {
                label: label.to_owned(),
                existing: existing.label.clone(),
                reason: DuplicateReason::SameAccount,
            });
            continue;
        }
        let mut final_label = label.to_owned();
        if let Some(existing) = config
            .accounts
            .iter()
            .find(|existing| existing.label.trim().to_lowercase() == label.to_lowercase())
        {
            match policy {
                DuplicatePolicy::Skip => {
                    by_label.insert(label.to_lowercase(), existing.id.clone());
                    report.duplicates.push(ImportDuplicate {
                        label: label.to_owned(),
                        existing: existing.label.clone(),
                        reason: DuplicateReason::Label,
                    });
                    continue;
                }
                DuplicatePolicy::Rename => {
                    report.duplicates.push(ImportDuplicate {
                        label: label.to_owned(),
                        existing: existing.label.clone(),
                        reason: DuplicateReason::Label,
                    });
                    final_label = free_label(config, label)?;
                }
            }
        }
        let source = match &incoming.source {
            ExportedSource::StaticKeys => CredentialSource::StaticKeys,
            ExportedSource::AwsProfile { profile } => CredentialSource::AwsProfile {
                profile: profile.clone(),
            },
            ExportedSource::AssumeRole {
                source_account,
                role_arn,
                session_name,
                duration_secs,
                external_id,
                mfa_serial,
            } => {
                let Some(source_id) = by_label.get(&source_account.trim().to_lowercase()) else {
                    // The duplicate entry above is not an import: take it back.
                    if policy == DuplicatePolicy::Rename
                        && final_label != label
                        && let Some(at) = report
                            .duplicates
                            .iter()
                            .rposition(|d| d.label == label && d.reason == DuplicateReason::Label)
                    {
                        report.duplicates.remove(at);
                    }
                    report.skipped.push(ImportSkipped {
                        label: label.to_owned(),
                        reason: format!(
                            "its source account {source_account:?} is not in the file or here"
                        ),
                    });
                    continue;
                };
                CredentialSource::AssumeRole {
                    source_account_id: source_id.clone(),
                    role_arn: role_arn.clone(),
                    session_name: session_name.clone(),
                    duration_secs: *duration_secs,
                    external_id: external_id.clone(),
                    mfa_serial: mfa_serial.clone(),
                }
            }
            ExportedSource::Sso {
                start_url,
                sso_region,
                account_id,
                role_name,
                sso_session,
            } => CredentialSource::Sso {
                start_url: start_url.clone(),
                sso_region: sso_region.clone(),
                account_id: account_id.clone(),
                role_name: role_name.clone(),
                sso_session: sso_session.clone(),
            },
            ExportedSource::InstanceRole => CredentialSource::InstanceRole,
        };
        if !source.is_static()
            && let Err(error) = check_source(config, &source, None)
        {
            report.skipped.push(ImportSkipped {
                label: label.to_owned(),
                reason: error.to_string(),
            });
            continue;
        }
        let id = new_uuid_v4()?;
        let mfa_serial = source.role_mfa_serial().map(str::to_owned).or_else(|| {
            incoming
                .mfa_serial
                .as_deref()
                .filter(|s| !s.is_empty() && source.is_static())
                .map(str::to_owned)
        });
        let account = Account {
            id: id.clone(),
            label: final_label.clone(),
            access_key_id: if source.is_static() {
                incoming.access_key_id.trim().to_owned()
            } else {
                String::new()
            },
            secret_ref: String::new(),
            session_token_ref: None,
            region: incoming.region.clone(),
            endpoint: incoming
                .endpoint
                .as_deref()
                .filter(|e| !e.is_empty())
                .map(str::to_owned),
            path_style: incoming.path_style,
            request_payer: incoming.request_payer,
            mfa_serial,
            source,
        };
        by_label.insert(label.to_lowercase(), id.clone());
        report.imported.push(ImportedAccount {
            id: id.clone(),
            label: final_label.clone(),
            renamed_from: (final_label != label).then(|| label.to_owned()),
            needs_secret: account.has_static_keys(),
        });
        if let Some(folder) = incoming.folder.as_deref() {
            folder_members
                .entry(check_folder_name(folder)?.to_owned())
                .or_default()
                .push(id.clone());
        }
        config.accounts.push(account);
        if config.default_account_id.is_none() {
            config.default_account_id = Some(id);
        }
    }
    // Folders: the file's folders keep their order after the existing ones.
    let mut wanted: Vec<String> = export.folders.clone();
    wanted.extend(folder_members.keys().cloned());
    for name in wanted {
        let Ok(name) = check_folder_name(&name) else {
            continue;
        };
        let at = config
            .account_folders
            .iter()
            .position(|f| f.name.to_lowercase() == name.to_lowercase());
        let at = match at {
            Some(at) => at,
            None => {
                config.account_folders.push(AccountFolder {
                    name: name.to_owned(),
                    account_ids: Vec::new(),
                });
                report.folders_created.push(name.to_owned());
                config.account_folders.len() - 1
            }
        };
        if let Some(members) = folder_members.remove(name) {
            config.account_folders[at].account_ids.extend(members);
        }
    }
    Ok(report)
}

impl AccountManager {
    /// Every account, in order, with the folders. Works while locked.
    pub fn layout(&self) -> Result<AccountLayout> {
        Ok(layout_of(&self.config_store().load()?))
    }

    /// Stores a new arrangement of folders and accounts (see
    /// [`apply_layout`]) and returns the account list. Touches no secret, so
    /// it works while locked.
    pub fn set_layout(&self, layout: &AccountLayout) -> Result<AccountList> {
        let _change = self.change_guard()?;
        let config = self.persist(|config| apply_layout(config, layout))?;
        Ok(list_of(config))
    }

    /// The export of the named accounts (all for `None`). Reads the
    /// configuration only: no secret is read, and it works while locked.
    pub fn export_accounts(&self, ids: Option<&[String]>) -> Result<AccountExport> {
        build_export(&self.config_store().load()?, ids)
    }

    /// Imports accounts from an export (see the [module docs](self)). No
    /// secret is read or written, so it works while locked; imported
    /// accounts with static keys need their secret entered afterwards.
    pub fn import_accounts(
        &self,
        export: &AccountExport,
        policy: DuplicatePolicy,
    ) -> Result<ImportReport> {
        let mut report = None;
        let _change = self.change_guard()?;
        self.persist(|config| {
            report = Some(apply_import(config, export, policy)?);
            Ok(())
        })?;
        report.ok_or_else(|| Error::Internal("the import did not run".into()))
    }
}

#[cfg(test)]
mod tests;
