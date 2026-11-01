//! Bookmarks (task T14): saved locations (account, bucket, prefix) with a
//! label, kept in `config.toml`.
//!
//! A bookmark points at a bucket (empty prefix), a folder (prefix ending in
//! `/`) or an object (its key). Every change is a read-modify-write under the
//! configuration lock ([`ConfigStore::update`]). Labels are trimmed, 1 to
//! [`MAX_LABEL_CHARS`] characters and free of control characters; without a
//! label the location itself (`bucket/prefix`) is used. The same location can
//! be bookmarked once per account. The list keeps the order of creation, and
//! at most [`MAX_BOOKMARKS`] can be stored. Removing an account drops its
//! bookmarks (see [`crate::accounts::AccountManager::remove`]).
//!
//! All functions here block (they read and write the configuration file);
//! async callers use `spawn_blocking`.

use serde::{Deserialize, Serialize};

use crate::config::{ConfigStore, new_uuid_v4};
use crate::error::{Error, Result};

/// Longest bookmark label, in characters.
pub const MAX_LABEL_CHARS: usize = 100;

/// Most bookmarks that can be stored.
pub const MAX_BOOKMARKS: usize = 500;

/// A saved location.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    /// Random id, stable across renames.
    pub id: String,
    /// Account the location belongs to.
    pub account_id: String,
    /// Bucket name.
    pub bucket: String,
    /// Folder prefix or object key; empty for the bucket itself.
    pub prefix: String,
    /// Name shown in the UI.
    pub label: String,
}

/// The stored bookmarks, in order.
pub fn list(store: &ConfigStore) -> Result<Vec<Bookmark>> {
    Ok(store.load()?.bookmarks)
}

/// Bookmarks the location `bucket`/`prefix` of an account and returns the
/// stored list. `label` of `None` (or blank) uses the location as the label.
///
/// Fails with `usage` for an empty bucket or an invalid label, `not_found`
/// for an unknown account and `conflict` when the location is already
/// bookmarked or [`MAX_BOOKMARKS`] would be exceeded.
pub fn add(
    store: &ConfigStore,
    account_id: &str,
    bucket: &str,
    prefix: &str,
    label: Option<&str>,
) -> Result<Vec<Bookmark>> {
    if bucket.trim().is_empty() {
        return Err(Error::Usage("a bookmark needs a bucket".into()));
    }
    let label = match label.map(str::trim).filter(|label| !label.is_empty()) {
        Some(label) => valid_label(label)?,
        None => default_label(bucket, prefix),
    };
    let id = new_uuid_v4()?;
    store.update(|config| {
        if !config
            .accounts
            .iter()
            .any(|account| account.id == account_id)
        {
            return Err(Error::NotFound(format!(
                "there is no account '{account_id}'"
            )));
        }
        if let Some(existing) = config
            .bookmarks
            .iter()
            .find(|b| b.account_id == account_id && b.bucket == bucket && b.prefix == prefix)
        {
            return Err(Error::Conflict(format!(
                "this location is already bookmarked as '{}'",
                existing.label
            )));
        }
        if config.bookmarks.len() >= MAX_BOOKMARKS {
            return Err(Error::Conflict(format!(
                "at most {MAX_BOOKMARKS} bookmarks can be saved; delete one first"
            )));
        }
        config.bookmarks.push(Bookmark {
            id,
            account_id: account_id.to_owned(),
            bucket: bucket.to_owned(),
            prefix: prefix.to_owned(),
            label,
        });
        Ok(config.bookmarks.clone())
    })
}

/// Gives the bookmark `id` a new label and returns the stored list; `usage`
/// for an invalid label, `not_found` for an unknown id.
pub fn rename(store: &ConfigStore, id: &str, label: &str) -> Result<Vec<Bookmark>> {
    let label = valid_label(label.trim())?;
    store.update(|config| {
        let bookmark = config
            .bookmarks
            .iter_mut()
            .find(|b| b.id == id)
            .ok_or_else(|| missing(id))?;
        bookmark.label = label;
        Ok(config.bookmarks.clone())
    })
}

/// Removes the bookmark `id` and returns the stored list; `not_found` for an
/// unknown id.
pub fn remove(store: &ConfigStore, id: &str) -> Result<Vec<Bookmark>> {
    store.update(|config| {
        let at = config
            .bookmarks
            .iter()
            .position(|b| b.id == id)
            .ok_or_else(|| missing(id))?;
        config.bookmarks.remove(at);
        Ok(config.bookmarks.clone())
    })
}

fn valid_label(label: &str) -> Result<String> {
    let length = label.chars().count();
    if length == 0 {
        return Err(Error::Usage("the bookmark needs a label".into()));
    }
    if length > MAX_LABEL_CHARS {
        return Err(Error::Usage(format!(
            "the label is {length} characters long; the maximum is {MAX_LABEL_CHARS}"
        )));
    }
    if label.chars().any(char::is_control) {
        return Err(Error::Usage(
            "the label must not contain control characters".into(),
        ));
    }
    Ok(label.to_owned())
}

/// The label used when none is given: `bucket` or `bucket/prefix`, cut to the
/// label limit.
fn default_label(bucket: &str, prefix: &str) -> String {
    let text: String = if prefix.is_empty() {
        bucket.to_owned()
    } else {
        format!("{bucket}/{prefix}")
    };
    let text: String = text.chars().filter(|c| !c.is_control()).collect();
    text.chars().take(MAX_LABEL_CHARS).collect()
}

fn missing(id: &str) -> Error {
    Error::NotFound(format!("there is no bookmark '{id}'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::error::ErrorKind;
    use crate::model::Account;

    fn store_with_accounts(ids: &[&str]) -> (tempfile::TempDir, ConfigStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::at(dir.path().join("config.toml"));
        store
            .update(|config: &mut AppConfig| {
                for id in ids {
                    config.accounts.push(Account {
                        id: (*id).to_owned(),
                        ..Account::default()
                    });
                }
                Ok(())
            })
            .unwrap();
        (dir, store)
    }

    #[test]
    fn add_uses_the_location_as_default_label_and_persists() {
        let (_dir, store) = store_with_accounts(&["a"]);
        let all = add(&store, "a", "data", "logs/2024/", None).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].label, "data/logs/2024/");
        let bucket = add(&store, "a", "data", "", Some("  Main  ")).unwrap();
        assert_eq!(bucket[1].label, "Main");
        assert_eq!(list(&store).unwrap(), bucket);
        assert!(
            std::fs::read_to_string(store.path())
                .unwrap()
                .contains("[[bookmarks]]")
        );
    }

    #[test]
    fn add_rejects_bad_input() {
        let (_dir, store) = store_with_accounts(&["a"]);
        let kind = |r: Result<Vec<Bookmark>>| r.unwrap_err().kind();
        assert_eq!(kind(add(&store, "a", " ", "", None)), ErrorKind::Usage);
        assert_eq!(kind(add(&store, "zz", "b", "", None)), ErrorKind::NotFound);
        assert_eq!(
            kind(add(&store, "a", "b", "", Some("x\ny"))),
            ErrorKind::Usage
        );
        let long = "x".repeat(MAX_LABEL_CHARS + 1);
        assert_eq!(
            kind(add(&store, "a", "b", "", Some(&long))),
            ErrorKind::Usage
        );
        add(&store, "a", "b", "p/", None).unwrap();
        assert_eq!(
            kind(add(&store, "a", "b", "p/", Some("other"))),
            ErrorKind::Conflict
        );
        assert_eq!(list(&store).unwrap().len(), 1);
    }

    #[test]
    fn the_same_location_can_be_bookmarked_in_two_accounts() {
        let (_dir, store) = store_with_accounts(&["a", "b"]);
        add(&store, "a", "data", "", None).unwrap();
        assert_eq!(add(&store, "b", "data", "", None).unwrap().len(), 2);
    }

    #[test]
    fn rename_and_remove() {
        let (_dir, store) = store_with_accounts(&["a"]);
        let all = add(&store, "a", "b1", "", None).unwrap();
        let all = add(&store, "a", "b2", "", None).unwrap_or(all);
        let first = all[0].id.clone();
        let renamed = rename(&store, &first, " Reports ").unwrap();
        assert_eq!(renamed[0].label, "Reports");
        assert_eq!(renamed[0].id, first);
        assert_eq!(
            rename(&store, &first, "  ").unwrap_err().kind(),
            ErrorKind::Usage
        );
        assert_eq!(
            rename(&store, "nope", "x").unwrap_err().kind(),
            ErrorKind::NotFound
        );
        let left = remove(&store, &first).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].bucket, "b2");
        assert_eq!(
            remove(&store, &first).unwrap_err().kind(),
            ErrorKind::NotFound
        );
    }

    #[test]
    fn the_limit_is_enforced() {
        let (_dir, store) = store_with_accounts(&["a"]);
        store
            .update(|config| {
                for n in 0..MAX_BOOKMARKS {
                    config.bookmarks.push(Bookmark {
                        id: n.to_string(),
                        account_id: "a".into(),
                        bucket: format!("b{n}"),
                        prefix: String::new(),
                        label: format!("b{n}"),
                    });
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(
            add(&store, "a", "extra", "", None).unwrap_err().kind(),
            ErrorKind::Conflict
        );
    }
}
