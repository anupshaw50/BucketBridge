//! `bb://` links (docs/desktop-api.md, "Everyday use"): parsing, resolving
//! the account, and handing the result to the UI.
//!
//! A link only navigates. Parsing is strict and does not need the network or
//! the secrets, so a link works while the app is locked.

use bucketbridge_core::model::Account;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::pick_window;
use crate::state::AppState;

/// The URL scheme.
pub const SCHEME: &str = "bb";

/// Event telling the UI that a link waits for it.
pub const LINK_PENDING: &str = "link://pending";

/// Longest link accepted.
const MAX_LINK_LEN: usize = 4096;

/// Longest bucket name accepted (S3 allows 63; other providers differ).
const MAX_BUCKET_LEN: usize = 255;

/// What a link asks for, before the account is looked up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedLink {
    /// Account id or label, decoded.
    pub account: String,
    /// Bucket, when the link names one.
    pub bucket: Option<String>,
    /// Folder prefix: empty or ending with `/`.
    pub prefix: String,
}

/// What the UI does with a link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LinkAction {
    /// Open this location.
    Open {
        /// Account to switch to.
        account_id: String,
        /// Bucket to open, or `None` for the account's bucket list.
        bucket: Option<String>,
        /// Folder prefix.
        prefix: String,
    },
    /// The link was refused.
    Error {
        /// Why, for the user.
        message: String,
    },
}

/// A link waiting for the window with label `target`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingLink {
    pub target: String,
    pub action: LinkAction,
}

/// Parses a `bb://` link. The error text is shown to the user.
pub(crate) fn parse_link(link: &str) -> Result<ParsedLink, String> {
    if link.len() > MAX_LINK_LEN {
        return Err("the link is too long".to_owned());
    }
    let link = link.trim();
    let rest = strip_scheme(link).ok_or_else(|| "the link does not start with bb://".to_owned())?;
    if rest.contains('?') || rest.contains('#') {
        return Err("the link must not contain a query or a fragment".to_owned());
    }
    let mut segments = rest.split('/');
    let account = decode_segment(segments.next().unwrap_or_default(), "the account")?;
    if account.is_empty() {
        return Err("the link names no account".to_owned());
    }
    let tail: Vec<&str> = segments.collect();
    // One trailing slash is allowed (`bb://a/b/`); it is not an empty segment.
    let tail = match tail.split_last() {
        Some((&"", rest)) => rest,
        _ => &tail[..],
    };
    let Some((bucket, folders)) = tail.split_first() else {
        return Ok(ParsedLink {
            account,
            bucket: None,
            prefix: String::new(),
        });
    };
    let bucket = decode_segment(bucket, "the bucket")?;
    if bucket.is_empty() || bucket == "." || bucket == ".." {
        return Err("the bucket name is not valid".to_owned());
    }
    if bucket.chars().count() > MAX_BUCKET_LEN {
        return Err("the bucket name is too long".to_owned());
    }
    let mut prefix = String::new();
    for folder in folders {
        let folder = decode_segment(folder, "a folder")?;
        if folder.is_empty() || folder == "." || folder == ".." {
            return Err("the folder path is not valid".to_owned());
        }
        if folder.contains('/') {
            return Err("a folder name must not contain an encoded slash".to_owned());
        }
        prefix.push_str(&folder);
        prefix.push('/');
    }
    if bucket.contains('/') {
        return Err("the bucket name must not contain an encoded slash".to_owned());
    }
    Ok(ParsedLink {
        account,
        bucket: Some(bucket),
        prefix,
    })
}

/// What follows `bb://`, when `link` starts with the scheme (any case).
fn strip_scheme(link: &str) -> Option<&str> {
    let (scheme, rest) = link.split_once("://")?;
    scheme.eq_ignore_ascii_case(SCHEME).then_some(rest)
}

/// Percent-decodes one segment; refuses invalid UTF-8 and control characters.
fn decode_segment(segment: &str, what: &str) -> Result<String, String> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = segment
                .get(i + 1..i + 3)
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .ok_or_else(|| format!("{what} has a broken percent escape"))?;
            out.push(hex);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let text = String::from_utf8(out).map_err(|_| format!("{what} is not valid UTF-8"))?;
    if text.chars().any(char::is_control) {
        return Err(format!("{what} contains a control character"));
    }
    Ok(text)
}

/// Looks the account up (id first, then label, case-insensitive) and builds
/// the action.
pub(crate) fn resolve(parsed: ParsedLink, accounts: &[Account]) -> LinkAction {
    let found = accounts
        .iter()
        .find(|account| account.id == parsed.account)
        .or_else(|| {
            let wanted = parsed.account.to_lowercase();
            accounts
                .iter()
                .find(|account| account.label.to_lowercase() == wanted)
        });
    match found {
        Some(account) => LinkAction::Open {
            account_id: account.id.clone(),
            bucket: parsed.bucket,
            prefix: parsed.prefix,
        },
        None => LinkAction::Error {
            message: format!("the link names an unknown account: {}", parsed.account),
        },
    }
}

/// Handles one received link: resolves it, stores it for the window that
/// takes it and announces it. Only the last link of a burst is kept.
pub(crate) fn handle_link<R: Runtime>(app: &AppHandle<R>, link: &str) {
    let action = match parse_link(link) {
        Err(message) => LinkAction::Error { message },
        Ok(parsed) => match app.try_state::<AppState>().map(|state| state.core()) {
            Some(Ok(core)) => match core.session.accounts().list() {
                Ok(list) => resolve(parsed, &list.accounts),
                Err(error) => LinkAction::Error {
                    message: error.to_string(),
                },
            },
            _ => LinkAction::Error {
                message: "BucketBridge is not ready to open links".to_owned(),
            },
        },
    };
    let target =
        pick_window(app).map_or_else(|| crate::MAIN_WINDOW.to_owned(), |w| w.label().to_owned());
    if let Some(Ok(core)) = app.try_state::<AppState>().map(|state| state.core()) {
        core.lifecycle.set_pending_link(PendingLink {
            target: target.clone(),
            action,
        });
    }
    crate::focus_main_window(app);
    let _ = app.emit_to(target, LINK_PENDING, ());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(link: &str) -> ParsedLink {
        parse_link(link).unwrap_or_else(|e| panic!("{link}: {e}"))
    }

    fn account(id: &str, label: &str) -> Account {
        Account {
            id: id.to_owned(),
            label: label.to_owned(),
            ..serde_json::from_value(serde_json::json!({
                "id": "x", "label": "x", "access_key_id": "k", "secret_ref": "s"
            }))
            .unwrap()
        }
    }

    #[test]
    fn parses_account_bucket_and_folders() {
        assert_eq!(
            ok("bb://work"),
            ParsedLink {
                account: "work".into(),
                bucket: None,
                prefix: String::new()
            }
        );
        assert_eq!(ok("bb://work/").bucket, None);
        assert_eq!(ok("bb://work/data").bucket.as_deref(), Some("data"));
        assert_eq!(ok("bb://work/data/").prefix, "");
        assert_eq!(ok("bb://work/data/a/b").prefix, "a/b/");
        assert_eq!(ok("bb://work/data/a/b/").prefix, "a/b/");
        assert_eq!(ok("BB://work/data").account, "work");
    }

    #[test]
    fn decodes_percent_escapes() {
        let link = ok("bb://My%20Backups/photos/trips/rome%202024/%C3%A9t%C3%A9/");
        assert_eq!(link.account, "My Backups");
        assert_eq!(link.prefix, "trips/rome 2024/\u{e9}t\u{e9}/");
        // Reserved characters survive only when encoded.
        assert_eq!(ok("bb://a/b/x%26y%3Dz").prefix, "x&y=z/");
    }

    #[test]
    fn refuses_bad_links() {
        for (link, why) in [
            ("https://work/data", "scheme"),
            ("bbx://work", "scheme"),
            ("bb:work", "scheme"),
            ("bb://", "no account"),
            ("bb:///data", "no account"),
            ("bb://work/data?x=1", "query"),
            ("bb://work/data#top", "fragment"),
            ("bb://work//data", "empty bucket"),
            ("bb://work/data//x", "empty segment"),
            ("bb://work/data/../x", "dotdot"),
            ("bb://work/data/./x", "dot"),
            ("bb://work/..", "dotdot bucket"),
            ("bb://work/data/a%2Fb", "encoded slash"),
            ("bb://work/data/%zz", "bad escape"),
            ("bb://work/data/%4", "short escape"),
            ("bb://work/data/%FF", "bad utf8"),
            ("bb://work/data/a%00b", "nul"),
            ("bb://work/data/a%0Ab", "newline"),
            ("bb://wo%0Ark/data", "control in account"),
        ] {
            assert!(parse_link(link).is_err(), "{why}: {link}");
        }
        let long = format!("bb://work/{}", "a".repeat(5000));
        assert!(parse_link(&long).is_err());
        let bucket = format!("bb://work/{}", "b".repeat(256));
        assert!(parse_link(&bucket).is_err());
    }

    #[test]
    fn resolves_by_id_then_label() {
        let accounts = [account("id-1", "Work"), account("id-2", "id-1x")];
        let parsed = |name: &str| ParsedLink {
            account: name.to_owned(),
            bucket: Some("data".into()),
            prefix: "a/".into(),
        };
        let open = |id: &str| LinkAction::Open {
            account_id: id.to_owned(),
            bucket: Some("data".into()),
            prefix: "a/".into(),
        };
        assert_eq!(resolve(parsed("id-1"), &accounts), open("id-1"));
        assert_eq!(resolve(parsed("work"), &accounts), open("id-1"));
        assert_eq!(resolve(parsed("ID-1X"), &accounts), open("id-2"));
        assert!(matches!(
            resolve(parsed("other"), &accounts),
            LinkAction::Error { .. }
        ));
    }

    #[test]
    fn actions_serialize_for_the_ui() {
        let open = LinkAction::Open {
            account_id: "a".into(),
            bucket: None,
            prefix: String::new(),
        };
        assert_eq!(
            serde_json::to_value(open).unwrap(),
            serde_json::json!({"type": "open", "account_id": "a", "bucket": null, "prefix": ""})
        );
        let error = LinkAction::Error {
            message: "no".into(),
        };
        assert_eq!(
            serde_json::to_value(error).unwrap(),
            serde_json::json!({"type": "error", "message": "no"})
        );
    }
}
