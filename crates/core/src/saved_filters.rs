//! Saved object filters (task T12, "Filters"): the list kept in
//! `config.toml` and the rules for changing it.
//!
//! A saved filter is a name and a [`FilterSpec`](crate::model::FilterSpec). The
//! spec is compiled by the filter engine ([`Filter::compile`]) before it is
//! stored, so a glob that does not parse, reversed size bounds, an empty date
//! interval or an empty storage class never reach the file. Every change is a
//! read-modify-write under the configuration lock ([`ConfigStore::update`]),
//! so the desktop app and any other process never lose each other's changes.
//!
//! Names are trimmed, 1 to [`MAX_NAME_CHARS`] characters, free of control
//! characters and unique ignoring case (Unicode lowercase). The list is kept
//! in the order of creation; replacing a filter keeps its place. At most
//! [`MAX_FILTERS`] filters can be stored.
//!
//! All functions here block (they read and write the configuration file);
//! async callers use `spawn_blocking`.

use crate::config::ConfigStore;
use crate::error::{Error, Result};
use crate::filter::Filter;
use crate::model::SavedFilter;

/// Longest filter name, in characters.
pub const MAX_NAME_CHARS: usize = 100;

/// Most filters that can be stored.
pub const MAX_FILTERS: usize = 200;

/// The stored filters, in order.
pub fn list(store: &ConfigStore) -> Result<Vec<SavedFilter>> {
    Ok(store.load()?.saved_filters)
}

/// Adds `filter` (`replaces` is `None`) or replaces the filter named
/// `replaces`, keeping its place; a replacement may rename it. Returns the
/// stored list.
///
/// Fails with `usage` for an invalid name or spec, with `conflict` when the
/// name belongs to another filter (or [`MAX_FILTERS`] would be exceeded), and
/// with `not_found` when there is no filter named `replaces`.
pub fn save(
    store: &ConfigStore,
    filter: SavedFilter,
    replaces: Option<&str>,
) -> Result<Vec<SavedFilter>> {
    let filter = validate(filter)?;
    store.update(|config| {
        let filters = &mut config.saved_filters;
        let position = match replaces {
            Some(old) => Some(position_of(filters, old).ok_or_else(|| missing(old))?),
            None => None,
        };
        if let Some(other) = position_of(filters, &filter.name)
            && Some(other) != position
        {
            return Err(Error::Conflict(format!(
                "a filter named '{}' already exists",
                filters[other].name
            )));
        }
        match position {
            Some(at) => filters[at] = filter,
            None if filters.len() >= MAX_FILTERS => {
                return Err(Error::Conflict(format!(
                    "at most {MAX_FILTERS} filters can be saved; delete one first"
                )));
            }
            None => filters.push(filter),
        }
        Ok(filters.clone())
    })
}

/// Removes the filter named `name` (ignoring case) and returns the stored
/// list; `not_found` when there is none.
pub fn delete(store: &ConfigStore, name: &str) -> Result<Vec<SavedFilter>> {
    store.update(|config| {
        let at = position_of(&config.saved_filters, name).ok_or_else(|| missing(name))?;
        config.saved_filters.remove(at);
        Ok(config.saved_filters.clone())
    })
}

/// Checks a filter and returns it in the form that is stored: the name
/// trimmed, an empty prefix or glob dropped, storage classes trimmed and
/// listed once (ignoring case).
pub fn validate(filter: SavedFilter) -> Result<SavedFilter> {
    let name = filter.name.trim();
    let length = name.chars().count();
    if length == 0 {
        return Err(Error::Usage("the filter needs a name".into()));
    }
    if length > MAX_NAME_CHARS {
        return Err(Error::Usage(format!(
            "the filter name is {length} characters long; the maximum is {MAX_NAME_CHARS}"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(Error::Usage(
            "the filter name must not contain control characters".into(),
        ));
    }
    let mut spec = filter.filter;
    spec.prefix = spec.prefix.filter(|prefix| !prefix.is_empty());
    spec.glob = spec.glob.filter(|glob| !glob.is_empty());
    let mut classes: Vec<String> = Vec::with_capacity(spec.storage_classes.len());
    for class in &spec.storage_classes {
        let class = class.trim();
        if !classes.iter().any(|seen| seen.eq_ignore_ascii_case(class)) {
            classes.push(class.to_owned());
        }
    }
    spec.storage_classes = classes;
    // Rejects what the engine rejects, an empty storage class name included.
    Filter::compile(&spec)?;
    Ok(SavedFilter {
        name: name.to_owned(),
        filter: spec,
    })
}

fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

fn position_of(filters: &[SavedFilter], name: &str) -> Option<usize> {
    let name = name.trim();
    filters
        .iter()
        .position(|filter| same_name(&filter.name, name))
}

fn missing(name: &str) -> Error {
    Error::NotFound(format!("there is no saved filter named '{}'", name.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::model::FilterSpec;
    use chrono::{TimeZone, Utc};

    fn store(dir: &tempfile::TempDir) -> ConfigStore {
        ConfigStore::at(dir.path().join("config.toml"))
    }

    fn named(name: &str) -> SavedFilter {
        SavedFilter {
            name: name.to_owned(),
            filter: FilterSpec {
                glob: Some("*.log".into()),
                ..FilterSpec::default()
            },
        }
    }

    fn names(filters: &[SavedFilter]) -> Vec<&str> {
        filters.iter().map(|f| f.name.as_str()).collect()
    }

    #[test]
    fn filters_are_stored_in_order_and_survive_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        assert!(list(&store).unwrap().is_empty());
        let big = SavedFilter {
            name: "Big logs".into(),
            filter: FilterSpec {
                prefix: Some("logs/".into()),
                glob: Some("**/*.gz".into()),
                min_size: Some(1024),
                max_size: Some(1 << 30),
                modified_after: Some(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()),
                modified_before: Some(Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap()),
                storage_classes: vec!["STANDARD_IA".into(), "GLACIER".into()],
            },
        };
        save(&store, named("Logs"), None).unwrap();
        let stored = save(&store, big.clone(), None).unwrap();
        assert_eq!(names(&stored), ["Logs", "Big logs"]);
        assert_eq!(stored[1], big);
        assert_eq!(list(&store).unwrap(), stored);
        // Another handle on the same file sees the same list.
        assert_eq!(list(&ConfigStore::at(store.path())).unwrap(), stored);
    }

    #[test]
    fn replacing_keeps_the_place_and_may_rename() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        for name in ["a", "b", "c"] {
            save(&store, named(name), None).unwrap();
        }
        let mut changed = named("B renamed");
        changed.filter.min_size = Some(5);
        let stored = save(&store, changed.clone(), Some("B")).unwrap();
        assert_eq!(names(&stored), ["a", "B renamed", "c"]);
        assert_eq!(stored[1], changed);
        // Saving under the same name replaces in place, too.
        let stored = save(&store, named("c"), Some("c")).unwrap();
        assert_eq!(names(&stored), ["a", "B renamed", "c"]);
        // Changing only the case of the name is allowed.
        let stored = save(&store, named("A"), Some("a")).unwrap();
        assert_eq!(names(&stored), ["A", "B renamed", "c"]);
    }

    #[test]
    fn names_are_unique_ignoring_case() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        save(&store, named("Reports"), None).unwrap();
        save(&store, named("Other"), None).unwrap();
        let err = save(&store, named("  rEPORTS "), None).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Conflict, "{err}");
        let err = save(&store, named("reports"), Some("Other")).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Conflict, "{err}");
        assert_eq!(names(&list(&store).unwrap()), ["Reports", "Other"]);
    }

    #[test]
    fn missing_filters_are_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        save(&store, named("one"), None).unwrap();
        for result in [
            save(&store, named("two"), Some("nope")),
            delete(&store, "nope"),
        ] {
            assert_eq!(result.unwrap_err().kind(), ErrorKind::NotFound);
        }
        let stored = delete(&store, " ONE ").unwrap();
        assert!(stored.is_empty());
        assert!(list(&store).unwrap().is_empty());
    }

    #[test]
    fn invalid_names_and_specs_are_usage_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let long = "x".repeat(MAX_NAME_CHARS + 1);
        for name in ["", "   ", "bad\nname", "tab\tname", long.as_str()] {
            let err = save(&store, named(name), None).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::Usage, "{name:?}: {err}");
        }
        // Exactly the longest name is fine (counted in characters).
        let longest = "\u{e9}".repeat(MAX_NAME_CHARS);
        save(&store, named(&longest), None).unwrap();

        let bad_specs = [
            FilterSpec {
                glob: Some("a[b".into()),
                ..FilterSpec::default()
            },
            FilterSpec {
                min_size: Some(10),
                max_size: Some(5),
                ..FilterSpec::default()
            },
            FilterSpec {
                modified_after: Some(Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap()),
                modified_before: Some(Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap()),
                ..FilterSpec::default()
            },
            FilterSpec {
                storage_classes: vec!["STANDARD".into(), "  ".into()],
                ..FilterSpec::default()
            },
        ];
        for spec in bad_specs {
            let err = save(
                &store,
                SavedFilter {
                    name: "bad".into(),
                    filter: spec,
                },
                None,
            )
            .unwrap_err();
            assert_eq!(err.kind(), ErrorKind::Usage, "{err}");
        }
        assert_eq!(list(&store).unwrap().len(), 1, "nothing invalid was stored");
    }

    #[test]
    fn stored_form_is_normalised() {
        let saved = validate(SavedFilter {
            name: "  tidy  ".into(),
            filter: FilterSpec {
                prefix: Some(String::new()),
                glob: Some(String::new()),
                storage_classes: vec![" standard ".into(), "STANDARD".into(), "Glacier".into()],
                ..FilterSpec::default()
            },
        })
        .unwrap();
        assert_eq!(saved.name, "tidy");
        assert_eq!(saved.filter.prefix, None);
        assert_eq!(saved.filter.glob, None);
        assert_eq!(saved.filter.storage_classes, ["standard", "Glacier"]);
    }

    #[test]
    fn the_list_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        store
            .update(|config| {
                config.saved_filters = (0..MAX_FILTERS).map(|i| named(&format!("f{i}"))).collect();
                Ok(())
            })
            .unwrap();
        let err = save(&store, named("one too many"), None).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Conflict, "{err}");
        // Replacing and deleting still work at the limit.
        save(&store, named("f0 again"), Some("f0")).unwrap();
        assert_eq!(delete(&store, "f1").unwrap().len(), MAX_FILTERS - 1);
        save(&store, named("one more"), None).unwrap();
    }

    #[test]
    fn changes_leave_the_rest_of_the_configuration_alone() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        store
            .update(|config| {
                config.settings.advanced.list_page_size = 77;
                Ok(())
            })
            .unwrap();
        save(&store, named("kept"), None).unwrap();
        let config = store.load().unwrap();
        assert_eq!(config.settings.advanced.list_page_size, 77);
        assert_eq!(names(&config.saved_filters), ["kept"]);
        // The stored file is the TOML array of tables the model documents.
        let text = std::fs::read_to_string(store.path()).unwrap();
        assert!(text.contains("[[saved_filters]]"), "{text}");
        assert!(text.contains("name = \"kept\""), "{text}");
    }

    #[test]
    fn a_broken_configuration_is_reported_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        std::fs::write(store.path(), "= =").unwrap();
        assert_eq!(list(&store).unwrap_err().kind(), ErrorKind::Io);
        assert_eq!(
            save(&store, named("x"), None).unwrap_err().kind(),
            ErrorKind::Io
        );
        assert_eq!(std::fs::read_to_string(store.path()).unwrap(), "= =");
    }
}
