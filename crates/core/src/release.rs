//! The opt-in check for a newer release (`bb version --check`).
//!
//! This is the only request BucketBridge makes that is not aimed at a
//! storage endpoint, and it is made only when the user asks for it: nothing
//! here runs on its own, at start-up or in the background. The request is a
//! single `GET` of the latest release of the project on GitHub; it carries no
//! account, no key, no identifier of the machine and no version number, only
//! a fixed `User-Agent` (GitHub rejects requests without one). It goes
//! through the proxy-aware HTTP client of the session, so the proxy settings
//! apply.
//!
//! | Request | Answer used |
//! |---|---|
//! | `GET https://api.github.com/repos/anupshaw50/BucketBridge/releases/latest` | `tag_name` and `html_url` |

use std::cmp::Ordering;

use aws_smithy_runtime_api::client::http::SharedHttpClient;
use aws_smithy_runtime_api::client::orchestrator::HttpRequest;
use serde_json::Value;

use crate::error::{Error, Result};

/// The API address of the latest release of the project.
pub const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/anupshaw50/BucketBridge/releases/latest";

/// What GitHub is told about the client. Fixed text: no version, no
/// platform.
const USER_AGENT: &str = "bucketbridge-update-check";

/// The longest answer read, in bytes (release notes can be long).
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// The latest release of the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestRelease {
    /// The tag as published, for example `v0.5.0`.
    pub tag: String,
    /// The version without a leading `v`, for example `0.5.0`.
    pub version: String,
    /// The page of the release.
    pub url: String,
}

/// Asks `url` (normally [`LATEST_RELEASE_URL`]; tests point it at a local
/// server) for the latest release.
///
/// Fails with `remote` when the server cannot be reached (including a
/// timeout), answers with another status than 200, or sends an answer that
/// is not a release; `auth` when the proxy rejects its credentials.
pub async fn latest_release(client: &SharedHttpClient, url: &str) -> Result<LatestRelease> {
    let mut request = HttpRequest::get(url)
        .map_err(|_| Error::Usage("the release address is not a valid URL".into()))?;
    let headers = request.headers_mut();
    headers.insert("accept", "application/vnd.github+json");
    headers.insert("user-agent", USER_AGENT);
    let (status, body) =
        crate::shorten::send(client, request, "GitHub", MAX_RESPONSE_BYTES).await?;
    match status {
        200 => parse_release(&body),
        404 => Err(Error::Remote(
            "GitHub has no published release of BucketBridge (HTTP 404)".into(),
        )),
        403 | 429 => Err(Error::Remote(format!(
            "GitHub is limiting requests (HTTP {status}): try again later"
        ))),
        _ => Err(Error::Remote(format!(
            "GitHub could not tell the latest release (HTTP {status})"
        ))),
    }
}

/// The release in the JSON answer of GitHub.
pub fn parse_release(body: &str) -> Result<LatestRelease> {
    let unusable = || Error::Remote("GitHub sent an answer that is not a release".into());
    let value: Value = serde_json::from_str(body).map_err(|_| unusable())?;
    let tag = value
        .get("tag_name")
        .and_then(Value::as_str)
        .filter(|tag| !tag.is_empty() && tag.len() <= 100 && !tag.chars().any(char::is_control))
        .ok_or_else(unusable)?;
    let url = value
        .get("html_url")
        .and_then(Value::as_str)
        .filter(|url| {
            url.starts_with("https://") && !url.chars().any(|c| c.is_control() || c.is_whitespace())
        })
        .ok_or_else(unusable)?;
    Ok(LatestRelease {
        tag: tag.to_owned(),
        version: tag.strip_prefix(['v', 'V']).unwrap_or(tag).to_owned(),
        url: url.to_owned(),
    })
}

/// A version such as `0.5.0`, `1.2.3-rc.1` (a leading `v` is ignored).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Version {
    numbers: Vec<u64>,
    pre: Option<String>,
}

impl Version {
    fn parse(text: &str) -> Option<Version> {
        let text = text.trim().strip_prefix(['v', 'V']).unwrap_or(text.trim());
        // Build metadata is ignored, as in semantic versioning.
        let text = text.split('+').next().unwrap_or(text);
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre.to_owned())),
            None => (text, None),
        };
        let numbers = core
            .split('.')
            .map(|part| part.parse::<u64>().ok())
            .collect::<Option<Vec<_>>>()?;
        if numbers.is_empty() || pre.as_deref() == Some("") {
            return None;
        }
        Some(Version { numbers, pre })
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let len = self.numbers.len().max(other.numbers.len());
        for index in 0..len {
            let a = self.numbers.get(index).copied().unwrap_or(0);
            let b = other.numbers.get(index).copied().unwrap_or(0);
            match a.cmp(&b) {
                Ordering::Equal => {}
                unequal => return unequal,
            }
        }
        // `1.0.0-rc.1` comes before `1.0.0`.
        match (&self.pre, &other.pre) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a), Some(b)) => compare_pre(a, b),
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Pre-release identifiers: numbers compare as numbers and come before
/// words, words compare as text; more identifiers win a tie.
fn compare_pre(a: &str, b: &str) -> Ordering {
    let mut left = a.split('.');
    let mut right = b.split('.');
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let order = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(x), Ok(y)) => x.cmp(&y),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => x.cmp(y),
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

/// How `latest` compares with the running `current` version: `Greater`
/// means an update is available. `None` when either is not a version.
pub fn compare_versions(latest: &str, current: &str) -> Option<Ordering> {
    Some(Version::parse(latest)?.cmp(&Version::parse(current)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_like_semver() {
        let newer = |latest: &str, current: &str| compare_versions(latest, current);
        assert_eq!(newer("v0.5.0", "0.4.0"), Some(Ordering::Greater));
        assert_eq!(newer("0.4.0", "0.4.0"), Some(Ordering::Equal));
        assert_eq!(newer("v0.4.0", "0.4.0"), Some(Ordering::Equal));
        assert_eq!(newer("0.3.9", "0.4.0"), Some(Ordering::Less));
        assert_eq!(newer("0.10.0", "0.9.0"), Some(Ordering::Greater));
        assert_eq!(newer("1.0", "1.0.0"), Some(Ordering::Equal));
        assert_eq!(newer("1.0.1", "1.0"), Some(Ordering::Greater));
        // A pre-release is before its release.
        assert_eq!(newer("1.0.0", "1.0.0-rc.1"), Some(Ordering::Greater));
        assert_eq!(newer("1.0.0-rc.1", "1.0.0"), Some(Ordering::Less));
        assert_eq!(newer("1.0.0-rc.2", "1.0.0-rc.1"), Some(Ordering::Greater));
        assert_eq!(newer("1.0.0-rc.10", "1.0.0-rc.9"), Some(Ordering::Greater));
        assert_eq!(newer("1.0.0-beta", "1.0.0-alpha"), Some(Ordering::Greater));
        // Build metadata does not count.
        assert_eq!(newer("1.0.0+abc", "1.0.0"), Some(Ordering::Equal));
        for bad in ["", "latest", "v", "1.x", "1..2", "1.0-"] {
            assert_eq!(newer(bad, "1.0.0"), None, "{bad}");
        }
    }

    #[test]
    fn the_release_comes_from_the_tag_and_the_page() {
        let release = parse_release(
            r#"{"tag_name":"v0.5.0","html_url":"https://github.com/anupshaw50/BucketBridge/releases/tag/v0.5.0","name":"x","body":"notes"}"#,
        )
        .unwrap();
        assert_eq!(release.tag, "v0.5.0");
        assert_eq!(release.version, "0.5.0");
        assert_eq!(
            release.url,
            "https://github.com/anupshaw50/BucketBridge/releases/tag/v0.5.0"
        );
        for bad in [
            "",
            "[]",
            "{}",
            r#"{"tag_name":"v1"}"#,
            r#"{"html_url":"https://x/y"}"#,
            r#"{"tag_name":"","html_url":"https://x/y"}"#,
            // Only https pages are shown.
            r#"{"tag_name":"v1","html_url":"javascript:alert(1)"}"#,
            r#"{"tag_name":"v1","html_url":"http://x/y"}"#,
            r#"{"tag_name":"v1","html_url":"https://x/y z"}"#,
            r#"{"tag_name":7,"html_url":"https://x/y"}"#,
        ] {
            let err = parse_release(bad).unwrap_err();
            assert_eq!(err.kind(), crate::ErrorKind::Remote, "{bad}");
        }
    }
}
