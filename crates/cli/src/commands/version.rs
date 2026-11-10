//! `bb version [--check]`: the running version, and, only when asked, whether
//! a newer release exists.
//!
//! Without `--check` nothing touches the network. With it, one request goes
//! to the GitHub API for the latest release (see
//! `bucketbridge_core::release`); it is the only request of `bb` that is not
//! aimed at a storage endpoint, and it is never made on its own.

use std::cmp::Ordering;

use bucketbridge_core::proxy::HttpClientBuilder;
use bucketbridge_core::release::{LATEST_RELEASE_URL, compare_versions, latest_release};
use bucketbridge_core::{Error, ErrorKind};

use crate::cli::{GlobalArgs, VersionArgs};
use crate::context::{Inputs, dirs, env, open_session, unlock};
use crate::i18n::trf;
use crate::output::{Failure, Output};
use crate::records::{RunningVersionRecord, UpdateCheckRecord};

/// The version of this `bb`.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Runs `bb version`.
pub async fn run(
    global: &GlobalArgs,
    args: &VersionArgs,
    inputs: &Inputs,
    out: &mut Output,
) -> Result<(), Failure> {
    if !args.check {
        let record = RunningVersionRecord {
            version: CURRENT_VERSION.to_owned(),
        };
        return out.row(&record, || format!("bb {CURRENT_VERSION}"));
    }
    let url = inputs
        .env
        .get(env::UPDATE_CHECK_URL)?
        .unwrap_or_else(|| LATEST_RELEASE_URL.to_owned());
    let latest = fetch(global, inputs, &url).await?;
    let ordering = compare_versions(&latest.version, CURRENT_VERSION).ok_or_else(|| {
        Error::Remote(trf(
            "the latest release has a version tag that is not understood: {tag}",
            &[("tag", &latest.tag)],
        ))
    })?;
    let record = UpdateCheckRecord {
        current: CURRENT_VERSION.to_owned(),
        latest: latest.version.clone(),
        update_available: ordering == Ordering::Greater,
        url: latest.url.clone(),
    };
    out.row(&record, || match ordering {
        Ordering::Greater => trf(
            "update available: {latest} (you have {current}); release page: {url}",
            &[
                ("latest", &latest.version),
                ("current", CURRENT_VERSION),
                ("url", &latest.url),
            ],
        ),
        Ordering::Equal => trf(
            "bb {current} is up to date (the latest release is {latest})",
            &[("current", CURRENT_VERSION), ("latest", &latest.version)],
        ),
        Ordering::Less => trf(
            "bb {current} is newer than the latest release ({latest})",
            &[("current", CURRENT_VERSION), ("latest", &latest.version)],
        ),
    })
}

/// Asks for the latest release through the proxy of the settings (and the
/// proxy environment variables). Without a configuration directory only the
/// environment variables apply.
async fn fetch(
    global: &GlobalArgs,
    inputs: &Inputs,
    url: &str,
) -> Result<bucketbridge_core::release::LatestRelease, Error> {
    let dirs = dirs(global, &inputs.env)?;
    let Some(dirs) = dirs else {
        let client = HttpClientBuilder::new().environment_proxy(true).build()?;
        return latest_release(&client, url).await;
    };
    let session = open_session(Some(&dirs), &inputs.secrets)?;
    match session.latest_release(url).await {
        // A proxy password under a master password needs it.
        Err(err) if err.kind() == ErrorKind::Locked => {
            unlock(&session, inputs).await?;
            session.latest_release(url).await
        }
        other => other,
    }
}
