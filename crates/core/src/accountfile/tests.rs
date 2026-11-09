use std::sync::Arc;

use super::*;
use crate::config::{ConfigStore, Dirs};
use crate::error::ErrorKind;
use crate::model::{AccountInput, AccountUpdate};
use crate::secrets::MemoryStore;

const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";

struct Fixture {
    dir: tempfile::TempDir,
    store: Arc<MemoryStore>,
    manager: AccountManager,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryStore::new());
        let manager = open(&dir, &store);
        Fixture {
            dir,
            store,
            manager,
        }
    }

    fn add(&self, label: &str, key: &str) -> Account {
        let mut input = AccountInput::default();
        input.label = label.to_owned();
        input.access_key_id = key.to_owned();
        input.secret_access_key = SECRET.to_owned();
        input.session_token = Some("a-session-token-value".to_owned());
        input.region = "eu-west-1".to_owned();
        self.manager.add(&input).unwrap()
    }

    fn ids(&self) -> Vec<String> {
        self.manager
            .list()
            .unwrap()
            .accounts
            .into_iter()
            .map(|a| a.id)
            .collect()
    }

    fn labels(&self) -> Vec<String> {
        self.manager
            .list()
            .unwrap()
            .accounts
            .into_iter()
            .map(|a| a.label)
            .collect()
    }
}

fn open(dir: &tempfile::TempDir, store: &Arc<MemoryStore>) -> AccountManager {
    let config = ConfigStore::new(&Dirs::under(dir.path()));
    AccountManager::open(config, store.clone()).unwrap()
}

fn folder(name: &str, ids: &[&String]) -> AccountFolder {
    AccountFolder {
        name: name.to_owned(),
        account_ids: ids.iter().map(|id| (*id).clone()).collect(),
    }
}

#[test]
fn an_old_configuration_without_folders_loads_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let old = "version = 1\ninstallation_id = \"install-0001\"\n\n[[accounts]]\nid = \"a\"\nlabel = \"L\"\naccess_key_id = \"K\"\nsecret_ref = \"a/secret/1\"\n";
    std::fs::write(dir.path().join("config.toml"), old).unwrap();
    let store = ConfigStore::new(&Dirs::under(dir.path()));
    let config = store.load().unwrap();
    assert!(config.account_folders.is_empty());
    assert_eq!(config.accounts.len(), 1);
    // Saving it again adds nothing about folders, and the list has none.
    store.update(|_| Ok(())).unwrap();
    let written = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(!written.contains("account_folders"), "{written}");
    let manager = AccountManager::open(store, Arc::new(MemoryStore::new())).unwrap();
    let list = manager.list().unwrap();
    assert!(list.folders.is_empty());
    assert_eq!(list.accounts[0].label, "L");
    // The JSON of a list without `folders` still parses.
    let parsed: AccountList =
        serde_json::from_str(r#"{"accounts":[],"default_account_id":null}"#).unwrap();
    assert!(parsed.folders.is_empty());
}

#[test]
fn the_layout_reorders_accounts_and_makes_folders() {
    let f = Fixture::new();
    let (a, b, c) = (
        f.add("A", "AKIAAAAAAAAAAAAAAAAA"),
        f.add("B", "AKIABBBBBBBBBBBBBBBB"),
        f.add("C", "AKIACCCCCCCCCCCCCCCC"),
    );
    let list = f
        .manager
        .set_layout(&AccountLayout {
            folders: vec![folder("Work", &[&c.id, &a.id]), folder("Empty", &[])],
            accounts: vec![c.id.clone(), b.id.clone(), a.id.clone()],
        })
        .unwrap();
    assert_eq!(
        list.accounts
            .iter()
            .map(|x| x.label.as_str())
            .collect::<Vec<_>>(),
        ["C", "B", "A"]
    );
    assert_eq!(list.folders.len(), 2);
    assert_eq!(list.folders[0].account_ids, [c.id.clone(), a.id.clone()]);
    // It survives a restart.
    let again = open(&f.dir, &f.store).list().unwrap();
    assert_eq!(again.folders, list.folders);
    assert_eq!(f.labels(), ["C", "B", "A"]);
    // Renaming and deleting a folder are layouts too.
    let list = f
        .manager
        .set_layout(&AccountLayout {
            folders: vec![folder("Job", &[&c.id])],
            accounts: vec![c.id.clone(), b.id.clone(), a.id.clone()],
        })
        .unwrap();
    assert_eq!(list.folders.len(), 1);
    assert_eq!(list.folders[0].name, "Job");
}

#[test]
fn a_bad_layout_changes_nothing() {
    let f = Fixture::new();
    let a = f.add("A", "AKIAAAAAAAAAAAAAAAAA");
    let b = f.add("B", "AKIABBBBBBBBBBBBBBBB");
    let before = f.manager.list().unwrap();
    let both = vec![a.id.clone(), b.id.clone()];
    let bad = [
        // An account is missing, or unknown.
        AccountLayout {
            folders: vec![],
            accounts: vec![a.id.clone()],
        },
        AccountLayout {
            folders: vec![],
            accounts: vec![a.id.clone(), b.id.clone(), "ghost".into()],
        },
        // Listed twice.
        AccountLayout {
            folders: vec![],
            accounts: vec![a.id.clone(), a.id.clone()],
        },
        // One account in two folders.
        AccountLayout {
            folders: vec![folder("X", &[&a.id]), folder("Y", &[&a.id])],
            accounts: both.clone(),
        },
        // The same folder name twice (case-insensitively).
        AccountLayout {
            folders: vec![folder("X", &[]), folder("x", &[])],
            accounts: both.clone(),
        },
        // An empty name.
        AccountLayout {
            folders: vec![folder("  ", &[])],
            accounts: both.clone(),
        },
    ];
    for layout in bad {
        assert!(f.manager.set_layout(&layout).is_err(), "{layout:?}");
    }
    assert_eq!(f.manager.list().unwrap(), before);
}

#[test]
fn removing_an_account_takes_it_out_of_its_folder() {
    let f = Fixture::new();
    let a = f.add("A", "AKIAAAAAAAAAAAAAAAAA");
    let b = f.add("B", "AKIABBBBBBBBBBBBBBBB");
    f.manager
        .set_layout(&AccountLayout {
            folders: vec![folder("Work", &[&a.id, &b.id])],
            accounts: f.ids(),
        })
        .unwrap();
    f.manager.remove(&a.id).unwrap();
    let list = f.manager.list().unwrap();
    assert_eq!(list.folders[0].account_ids, [b.id]);
}

#[test]
fn the_export_has_no_secret_and_keeps_order_and_folders() {
    let f = Fixture::new();
    let a = f.add("A", "AKIAAAAAAAAAAAAAAAAA");
    let b = f.add("B", "AKIABBBBBBBBBBBBBBBB");
    f.manager
        .set_layout(&AccountLayout {
            folders: vec![folder("Work", &[&b.id])],
            accounts: vec![b.id.clone(), a.id.clone()],
        })
        .unwrap();
    let export = f.manager.export_accounts(None).unwrap();
    assert_eq!(export.format, EXPORT_FORMAT);
    assert_eq!(export.version, EXPORT_VERSION);
    assert_eq!(export.folders, ["Work"]);
    assert_eq!(export.accounts[0].label, "B");
    assert_eq!(export.accounts[0].folder.as_deref(), Some("Work"));
    assert_eq!(export.accounts[1].folder, None);
    let file = f.dir.path().join("accounts.json");
    write_export(&export, &file).unwrap();
    let text = std::fs::read_to_string(&file).unwrap();
    for forbidden in [
        SECRET,
        "a-session-token-value",
        "secret_ref",
        "session_token",
        "/secret/",
    ] {
        assert!(!text.contains(forbidden), "{forbidden} is in the file");
    }
    assert!(text.contains("AKIAAAAAAAAAAAAAAAAA"));
    assert_eq!(read_export(&file).unwrap().accounts, export.accounts);
    // A subset exports only what was asked for, with its folder.
    let one = f
        .manager
        .export_accounts(Some(std::slice::from_ref(&a.id)))
        .unwrap();
    assert_eq!(one.accounts.len(), 1);
    assert!(one.folders.is_empty());
    assert_eq!(
        f.manager
            .export_accounts(Some(&["nope".to_owned()]))
            .unwrap_err()
            .kind(),
        ErrorKind::NotFound
    );
}

#[test]
fn an_import_creates_accounts_that_need_a_secret_and_never_overwrites() {
    let source = Fixture::new();
    let a = source.add("A", "AKIAAAAAAAAAAAAAAAAA");
    let b = source.add("B", "AKIABBBBBBBBBBBBBBBB");
    source
        .manager
        .set_layout(&AccountLayout {
            folders: vec![folder("Work", &[&a.id]), folder("Home", &[&b.id])],
            accounts: source.ids(),
        })
        .unwrap();
    let export = source.manager.export_accounts(None).unwrap();

    // Into an empty installation: both arrive, in folders, needing a secret.
    let target = Fixture::new();
    let report = target
        .manager
        .import_accounts(&export, DuplicatePolicy::Skip)
        .unwrap();
    assert_eq!(report.imported.len(), 2);
    assert!(report.imported.iter().all(|i| i.needs_secret));
    assert!(report.duplicates.is_empty() && report.skipped.is_empty());
    assert_eq!(report.folders_created, ["Work", "Home"]);
    let list = target.manager.list().unwrap();
    assert_eq!(list.folders.len(), 2);
    assert!(list.accounts.iter().all(Account::needs_secret));
    assert_eq!(target.store.len(), 0, "no secret was stored");
    // Such an account says what is missing, and a secret fixes it.
    let imported = &list.accounts[0];
    let error = target
        .manager
        .account_credentials(&imported.id)
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Usage);
    assert!(error.to_string().contains("secret"), "{error}");
    let mut update = AccountUpdate::default();
    update.secret_access_key = Some(SECRET.to_owned());
    target.manager.update(&imported.id, &update).unwrap();
    assert!(!target.manager.get(&imported.id).unwrap().needs_secret());
    assert!(target.manager.account_credentials(&imported.id).is_ok());

    // Importing the same file again overwrites nothing and reports duplicates.
    let before = target.manager.list().unwrap();
    let again = target
        .manager
        .import_accounts(&export, DuplicatePolicy::Skip)
        .unwrap();
    assert!(again.imported.is_empty());
    assert_eq!(again.duplicates.len(), 2);
    assert!(
        again
            .duplicates
            .iter()
            .all(|d| d.reason == DuplicateReason::SameAccount)
    );
    assert_eq!(target.manager.list().unwrap(), before);
}

#[test]
fn a_label_duplicate_is_skipped_or_renamed() {
    let source = Fixture::new();
    source.add("Shared", "AKIAAAAAAAAAAAAAAAAA");
    let export = source.manager.export_accounts(None).unwrap();

    let target = Fixture::new();
    target.add("shared", "AKIAZZZZZZZZZZZZZZZZ");
    let skipped = target
        .manager
        .import_accounts(&export, DuplicatePolicy::Skip)
        .unwrap();
    assert!(skipped.imported.is_empty());
    assert_eq!(skipped.duplicates[0].reason, DuplicateReason::Label);
    assert_eq!(skipped.duplicates[0].existing, "shared");
    assert_eq!(target.labels(), ["shared"]);

    let renamed = target
        .manager
        .import_accounts(&export, DuplicatePolicy::Rename)
        .unwrap();
    assert_eq!(renamed.imported.len(), 1);
    assert_eq!(renamed.imported[0].label, "Shared (2)");
    assert_eq!(renamed.imported[0].renamed_from.as_deref(), Some("Shared"));
    assert_eq!(renamed.duplicates.len(), 1);
    assert_eq!(target.labels(), ["shared", "Shared (2)"]);
}

#[test]
fn an_assumed_role_finds_its_source_account_by_label() {
    let source = Fixture::new();
    let base = source.add("Base", "AKIAAAAAAAAAAAAAAAAA");
    let mut role_input = AccountInput::default();
    role_input.label = "Role".into();
    role_input.region = "eu-west-1".into();
    role_input.source = CredentialSource::AssumeRole {
        source_account_id: base.id.clone(),
        role_arn: "arn:aws:iam::123456789012:role/Deploy".into(),
        session_name: "bucketbridge".into(),
        duration_secs: 3600,
        external_id: None,
        mfa_serial: None,
    };
    source.manager.add(&role_input).unwrap();
    // Exporting only the role brings its source account along.
    let role_id = source.ids()[1].clone();
    let export = source.manager.export_accounts(Some(&[role_id])).unwrap();
    assert_eq!(export.accounts.len(), 2);
    let text = serde_json::to_string(&export).unwrap();
    assert!(text.contains("\"source_account\":\"Base\""), "{text}");
    assert!(!text.contains(&base.id), "local ids stay out of the file");

    let target = Fixture::new();
    let report = target
        .manager
        .import_accounts(&export, DuplicatePolicy::Skip)
        .unwrap();
    assert_eq!(report.imported.len(), 2, "{report:?}");
    let accounts = target.manager.list().unwrap().accounts;
    let role = accounts.iter().find(|a| a.label == "Role").unwrap();
    let imported_base = accounts.iter().find(|a| a.label == "Base").unwrap();
    match &role.source {
        CredentialSource::AssumeRole {
            source_account_id, ..
        } => assert_eq!(source_account_id, &imported_base.id),
        other => panic!("{other:?}"),
    }
    assert!(!role.needs_secret());

    // A role whose source account is nowhere is skipped, not imported.
    let mut broken = export.clone();
    broken.accounts.remove(0);
    let lonely = Fixture::new()
        .manager
        .import_accounts(&broken, DuplicatePolicy::Skip)
        .unwrap();
    assert!(lonely.imported.is_empty());
    assert_eq!(lonely.skipped.len(), 1);
}

#[test]
fn invalid_entries_are_skipped_and_bad_files_refused() {
    let mut export = AccountExport {
        format: EXPORT_FORMAT.to_owned(),
        version: EXPORT_VERSION,
        exported_at: Utc::now(),
        folders: vec![],
        accounts: vec![ExportedAccount {
            label: "No key".into(),
            folder: None,
            access_key_id: String::new(),
            region: "eu-west-1".into(),
            endpoint: None,
            path_style: false,
            request_payer: false,
            mfa_serial: None,
            source: ExportedSource::StaticKeys,
        }],
    };
    let f = Fixture::new();
    let report = f
        .manager
        .import_accounts(&export, DuplicatePolicy::Skip)
        .unwrap();
    assert!(report.imported.is_empty());
    assert_eq!(report.skipped.len(), 1);
    export.accounts[0].access_key_id = "AKIAAAAAAAAAAAAAAAAA".into();
    export.accounts[0].region = "not a region!".into();
    assert_eq!(
        f.manager
            .import_accounts(&export, DuplicatePolicy::Skip)
            .unwrap()
            .skipped
            .len(),
        1
    );

    for text in [
        "not json",
        r#"{"format":"other","version":1}"#,
        r#"{"format":"bucketbridge-accounts","version":2,"exported_at":"2026-01-01T00:00:00Z","accounts":[]}"#,
        r#"{"format":"bucketbridge-accounts","exported_at":"2026-01-01T00:00:00Z","accounts":[]}"#,
        r#"{"format":"bucketbridge-accounts","version":1}"#,
    ] {
        assert_eq!(
            parse_export(text).unwrap_err().kind(),
            ErrorKind::Usage,
            "{text}"
        );
    }
    assert!(
        parse_export(
            r#"{"format":"bucketbridge-accounts","version":1,"exported_at":"2026-01-01T00:00:00Z","accounts":[]}"#
        )
        .is_ok()
    );
}

#[test]
fn a_master_password_change_ignores_accounts_that_need_a_secret() {
    let source = Fixture::new();
    source.add("A", "AKIAAAAAAAAAAAAAAAAA");
    let export = source.manager.export_accounts(None).unwrap();
    let target = Fixture::new();
    target
        .manager
        .import_accounts(&export, DuplicatePolicy::Skip)
        .unwrap();
    target.add("Own", "AKIAOOOOOOOOOOOOOOOO");
    target
        .manager
        .set_master_password(None, Some("correct horse battery"))
        .unwrap();
    target.manager.unlock("correct horse battery").unwrap();
    assert!(target.manager.list().unwrap().accounts[0].needs_secret());
}
