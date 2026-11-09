//! `bb account move|folder|export|import`: folders, the order of accounts,
//! and the account file (no secrets).

use std::path::Path;

use bucketbridge_core::accountfile::{
    AccountFolder, AccountLayout, DuplicatePolicy, DuplicateReason, read_export, write_export,
};
use bucketbridge_core::model::{Account, AccountList};
use bucketbridge_core::{Error, Result, Session};

use crate::cli::{AccountExportArgs, AccountFolderCommand, AccountImportArgs, AccountMoveArgs};
use crate::context::find_account;
use crate::i18n::{tr, trf};
use crate::output::{Failure, Output};
use crate::records::{
    AccountMoveRecord, ExportRecord, FolderRecord, ImportRecord, ImportSummaryRecord,
};

fn not_found(selector: &str) -> Error {
    Error::NotFound(trf(
        "account '{selector}' not found; `bb account list` shows the stored accounts",
        &[("selector", selector)],
    ))
}

fn layout_of(list: &AccountList) -> AccountLayout {
    AccountLayout {
        folders: list.folders.clone(),
        accounts: list.accounts.iter().map(|a| a.id.clone()).collect(),
    }
}

fn folder_position(layout: &AccountLayout, name: &str) -> Option<usize> {
    layout
        .folders
        .iter()
        .position(|folder| folder.name.to_lowercase() == name.trim().to_lowercase())
}

/// The name of the folder an account is in.
pub fn folder_of<'a>(list: &'a AccountList, id: &str) -> Option<&'a str> {
    list.folders
        .iter()
        .find(|folder| folder.account_ids.iter().any(|member| member == id))
        .map(|folder| folder.name.as_str())
}

/// `bb account move`
pub async fn move_account(
    session: &Session,
    args: &AccountMoveArgs,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    let list = session.list_accounts().await?;
    let account = find_account(&list.accounts, &args.label_or_id)
        .ok_or_else(|| not_found(&args.label_or_id))?
        .clone();
    let before = match &args.before {
        Some(selector) => Some(
            find_account(&list.accounts, selector)
                .ok_or_else(|| not_found(selector))?
                .id
                .clone(),
        ),
        None => None,
    };
    if before.as_deref() == Some(account.id.as_str()) {
        return Err(Error::Usage(tr("an account cannot be moved before itself")).into());
    }
    let mut layout = layout_of(&list);
    let mut member_of = list
        .folders
        .iter()
        .position(|folder| folder.account_ids.contains(&account.id));
    if args.no_folder || args.folder.is_some() {
        if let Some(at) = member_of {
            layout.folders[at]
                .account_ids
                .retain(|id| *id != account.id);
        }
        member_of = None;
    }
    if let Some(name) = &args.folder {
        let at = match folder_position(&layout, name) {
            Some(at) => at,
            None => {
                layout.folders.push(AccountFolder {
                    name: name.trim().to_owned(),
                    account_ids: Vec::new(),
                });
                layout.folders.len() - 1
            }
        };
        layout.folders[at].account_ids.push(account.id.clone());
        member_of = Some(at);
    }
    // Position in the list and, inside a folder, in the folder.
    if args.first || before.is_some() {
        layout.accounts.retain(|id| *id != account.id);
        let at = if args.first {
            0
        } else {
            before
                .as_ref()
                .and_then(|target| layout.accounts.iter().position(|id| id == target))
                .unwrap_or(layout.accounts.len())
        };
        layout.accounts.insert(at, account.id.clone());
        if let Some(folder_at) = member_of {
            let members = &mut layout.folders[folder_at].account_ids;
            members.retain(|id| *id != account.id);
            let inside = if args.first {
                0
            } else {
                before
                    .as_ref()
                    .and_then(|target| members.iter().position(|id| id == target))
                    .unwrap_or(members.len())
            };
            members.insert(inside, account.id.clone());
        }
    }
    let updated = session.set_account_layout(layout).await?;
    let record = AccountMoveRecord {
        id: account.id.clone(),
        label: account.label.clone(),
        folder: folder_of(&updated, &account.id).map(str::to_owned),
    };
    out.change(&record, || match &record.folder {
        Some(folder) => trf(
            "moved account {label} to folder {folder}",
            &[("label", record.label.as_str()), ("folder", folder)],
        ),
        None => trf(
            "moved account {label} (no folder)",
            &[("label", record.label.as_str())],
        ),
    })
}

/// `bb account folder ...`
pub async fn folder(
    session: &Session,
    command: &AccountFolderCommand,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    let list = session.list_accounts().await?;
    let mut layout = layout_of(&list);
    match command {
        AccountFolderCommand::List => {
            for folder in &list.folders {
                let record = FolderRecord {
                    name: folder.name.clone(),
                    account_ids: folder.account_ids.clone(),
                    accounts: labels_of(&list.accounts, &folder.account_ids),
                };
                out.row(&record, || {
                    format!("{}  ({})", record.name, record.accounts.join(", "))
                })?;
            }
            Ok(())
        }
        AccountFolderCommand::Add(args) => {
            if folder_position(&layout, &args.name).is_some() {
                return Err(Error::Conflict(trf(
                    "a folder named {name} already exists",
                    &[("name", args.name.as_str())],
                ))
                .into());
            }
            layout.folders.push(AccountFolder {
                name: args.name.trim().to_owned(),
                account_ids: Vec::new(),
            });
            let record = changed(&layout, &args.name, session).await?;
            out.change(&record, || {
                trf("added folder {name}", &[("name", record.name.as_str())])
            })
        }
        AccountFolderCommand::Rename(args) => {
            let at = folder_position(&layout, &args.old_name).ok_or_else(|| {
                Error::NotFound(trf(
                    "folder '{name}' not found; `bb account folder list` shows the folders",
                    &[("name", args.old_name.as_str())],
                ))
            })?;
            layout.folders[at].name = args.new_name.trim().to_owned();
            let record = changed(&layout, &args.new_name, session).await?;
            out.change(&record, || {
                trf(
                    "renamed folder {old} to {name}",
                    &[
                        ("old", args.old_name.as_str()),
                        ("name", record.name.as_str()),
                    ],
                )
            })
        }
        AccountFolderCommand::Remove(args) => {
            let at = folder_position(&layout, &args.name).ok_or_else(|| {
                Error::NotFound(trf(
                    "folder '{name}' not found; `bb account folder list` shows the folders",
                    &[("name", args.name.as_str())],
                ))
            })?;
            let removed = layout.folders.remove(at);
            session.set_account_layout(layout).await?;
            let record = FolderRecord {
                accounts: labels_of(&list.accounts, &removed.account_ids),
                name: removed.name,
                account_ids: removed.account_ids,
            };
            out.change(&record, || {
                trf(
                    "removed folder {name} (its accounts are kept)",
                    &[("name", record.name.as_str())],
                )
            })
        }
    }
}

async fn changed(layout: &AccountLayout, name: &str, session: &Session) -> Result<FolderRecord> {
    let updated = session.set_account_layout(layout.clone()).await?;
    let folder = updated
        .folders
        .iter()
        .find(|folder| folder.name.to_lowercase() == name.trim().to_lowercase())
        .ok_or_else(|| Error::Internal("the folder was not saved".into()))?;
    Ok(FolderRecord {
        name: folder.name.clone(),
        account_ids: folder.account_ids.clone(),
        accounts: labels_of(&updated.accounts, &folder.account_ids),
    })
}

fn labels_of(accounts: &[Account], ids: &[String]) -> Vec<String> {
    ids.iter()
        .filter_map(|id| accounts.iter().find(|account| account.id == *id))
        .map(|account| account.label.clone())
        .collect()
}

/// `bb account export`
pub async fn export(
    session: &Session,
    args: &AccountExportArgs,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    let list = session.list_accounts().await?;
    let ids = if args.accounts.is_empty() {
        None
    } else {
        let mut ids = Vec::new();
        for selector in &args.accounts {
            ids.push(
                find_account(&list.accounts, selector)
                    .ok_or_else(|| not_found(selector))?
                    .id
                    .clone(),
            );
        }
        Some(ids)
    };
    let export = session.export_accounts(ids).await?;
    let path = args.file.clone();
    let written = export.clone();
    tokio::task::spawn_blocking(move || write_export(&written, &path))
        .await
        .map_err(|err| Error::Internal(format!("a background task failed: {err}")))??;
    let record = ExportRecord {
        file: args.file.display().to_string(),
        accounts: export.accounts.len(),
        folders: export.folders.len(),
    };
    out.change(&record, || {
        trf(
            "wrote {count} account(s) to {file} (no secrets: imported accounts need their secret access key)",
            &[
                ("count", &record.accounts.to_string()),
                ("file", record.file.as_str()),
            ],
        )
    })
}

/// `bb account import`
pub async fn import(
    session: &Session,
    args: &AccountImportArgs,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    let path = args.file.clone();
    let export = tokio::task::spawn_blocking(move || read_export(Path::new(&path)))
        .await
        .map_err(|err| Error::Internal(format!("a background task failed: {err}")))??;
    let policy = if args.rename_duplicates {
        DuplicatePolicy::Rename
    } else {
        DuplicatePolicy::Skip
    };
    let report = session.import_accounts(export, policy).await?;
    // A renamed duplicate is also listed as imported: it is reported once.
    let renamed = |label: &str| {
        report
            .imported
            .iter()
            .any(|account| account.renamed_from.as_deref() == Some(label))
    };
    for account in &report.imported {
        let record = ImportRecord {
            kind: "imported",
            label: account.label.clone(),
            id: Some(account.id.clone()),
            renamed_from: account.renamed_from.clone(),
            needs_secret: Some(account.needs_secret),
            existing: None,
            reason: None,
        };
        out.change(&record, || {
            if account.needs_secret {
                trf(
                    "imported account {label}: it needs its secret access key before it can be used (bb account set-secret)",
                    &[("label", account.label.as_str())],
                )
            } else {
                trf(
                    "imported account {label}",
                    &[("label", account.label.as_str())],
                )
            }
        })?;
    }
    let mut duplicates = 0;
    for duplicate in report.duplicates.iter().filter(|d| !renamed(&d.label)) {
        duplicates += 1;
        let reason = match duplicate.reason {
            DuplicateReason::Label => "label",
            DuplicateReason::SameAccount => "same_account",
        };
        let record = ImportRecord {
            kind: "duplicate",
            label: duplicate.label.clone(),
            id: None,
            renamed_from: None,
            needs_secret: None,
            existing: Some(duplicate.existing.clone()),
            reason: Some(reason.to_owned()),
        };
        out.row(&record, || match duplicate.reason {
            DuplicateReason::Label => trf(
                "skipped {label}: an account with this label exists (--rename-duplicates imports it under another label)",
                &[("label", duplicate.label.as_str())],
            ),
            DuplicateReason::SameAccount => trf(
                "skipped {label}: it is the same account as {existing}",
                &[
                    ("label", duplicate.label.as_str()),
                    ("existing", duplicate.existing.as_str()),
                ],
            ),
        })?;
    }
    for skipped in &report.skipped {
        let record = ImportRecord {
            kind: "skipped",
            label: skipped.label.clone(),
            id: None,
            renamed_from: None,
            needs_secret: None,
            existing: None,
            reason: Some(skipped.reason.clone()),
        };
        out.row(&record, || {
            trf(
                "skipped {label}: {reason}",
                &[
                    ("label", skipped.label.as_str()),
                    ("reason", skipped.reason.as_str()),
                ],
            )
        })?;
    }
    let summary = ImportSummaryRecord {
        imported: report.imported.len(),
        duplicates,
        skipped: report.skipped.len(),
        folders_created: report.folders_created.clone(),
    };
    out.change(&summary, || {
        trf(
            "{imported} account(s) imported, {duplicates} duplicate(s) left out, {skipped} skipped",
            &[
                ("imported", &summary.imported.to_string()),
                ("duplicates", &summary.duplicates.to_string()),
                ("skipped", &summary.skipped.to_string()),
            ],
        )
    })
}
