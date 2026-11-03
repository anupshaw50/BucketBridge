//! The overwrite policy: what a transfer does when its destination exists,
//! and the modification-time metadata that "overwrite if newer" and
//! "keep modification times" share.
//!
//! The engines ask [`TransferEngine::place_local`] and
//! [`TransferEngine::place_remote`] before they write. With the default
//! policy ([`IfExists::Overwrite`]) nothing is checked and no request is made.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};

use super::TransferEngine;
use super::control::{Hooks, StopReason};
use crate::error::{Error, ErrorKind, Result};
use crate::model::{ObjectHead, TransferKind};
use crate::ops::S3;
use crate::s3uri::S3Uri;
use crate::settings::IfExists;

/// The user metadata key (`x-amz-meta-mtime`) that holds the local
/// modification time of an uploaded file.
pub const MTIME_META_KEY: &str = "mtime";

/// Most numbered names (`name (1)`, `name (2)`, ...) tried before giving up.
const MAX_RENAME_TRIES: u32 = 10_000;

/// An answer to a conflict: [`IfExists`] without `Ask`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Choice {
    /// Replace the destination.
    Overwrite,
    /// Keep the destination; the item is skipped.
    Skip,
    /// Replace the destination only when the source is newer.
    OverwriteIfNewer,
    /// Write under a free name.
    Rename,
}

impl From<Choice> for IfExists {
    fn from(choice: Choice) -> Self {
        match choice {
            Choice::Overwrite => IfExists::Overwrite,
            Choice::Skip => IfExists::Skip,
            Choice::OverwriteIfNewer => IfExists::OverwriteIfNewer,
            Choice::Rename => IfExists::Rename,
        }
    }
}

impl TryFrom<IfExists> for Choice {
    type Error = Error;

    /// `Ask` is no answer.
    fn try_from(policy: IfExists) -> Result<Self> {
        match policy {
            IfExists::Ask => Err(Error::Usage("'ask' is not an answer".into())),
            IfExists::Overwrite => Ok(Choice::Overwrite),
            IfExists::Skip => Ok(Choice::Skip),
            IfExists::OverwriteIfNewer => Ok(Choice::OverwriteIfNewer),
            IfExists::Rename => Ok(Choice::Rename),
        }
    }
}

/// Why an item was skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// The destination exists and the policy (or the answer) says to keep it.
    Exists,
    /// The destination exists and the source is not newer.
    NotNewer,
}

impl SkipReason {
    /// A sentence for logs and reports.
    pub fn describe(self) -> &'static str {
        match self {
            SkipReason::Exists => "the destination already exists",
            SkipReason::NotNewer => "the destination exists and the source is not newer",
        }
    }
}

/// What is known about one side of a conflict.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemInfo {
    /// Size in bytes.
    pub size: Option<u64>,
    /// Modification time (for an object: the stored `mtime` metadata, else
    /// `Last-Modified`).
    pub modified: Option<DateTime<Utc>>,
}

/// A destination that exists, to be decided on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    /// The operation.
    pub kind: TransferKind,
    /// The source, as a path or an `s3://` URI.
    pub source: String,
    /// The destination, as a path or an `s3://` URI.
    pub destination: String,
    /// The source.
    pub source_info: ItemInfo,
    /// What is already at the destination.
    pub destination_info: ItemInfo,
}

/// Decides conflicts for the policy [`IfExists::Ask`]. The desktop host
/// shows a dialog, the command line asks on the terminal.
pub trait ConflictResolver: Send + Sync {
    /// The answer for `conflict`. The engine drops the future when the
    /// transfer is stopped.
    fn resolve<'a>(&'a self, conflict: &'a Conflict) -> BoxFuture<'a, Result<Choice>>;
}

/// The result of placing an item.
#[derive(Debug)]
pub(crate) enum Placed<T> {
    /// Go on, writing to this destination (renamed when it is not the
    /// requested one).
    Go(T),
    /// Do not write.
    Skip(SkipReason),
    /// A stop arrived while waiting for an answer.
    Stop(Option<StopReason>),
}

impl TransferEngine {
    /// A copy of these engines that applies `policy` to every item whose
    /// destination exists (uploads, downloads, copies and moves; folder
    /// markers and folders merge and never conflict).
    #[must_use]
    pub fn with_if_exists(&self, policy: IfExists) -> Self {
        let mut engine = self.clone();
        engine.options.if_exists = policy;
        engine
    }

    /// A copy of these engines that asks `resolver` for the policy
    /// [`IfExists::Ask`].
    #[must_use]
    pub fn with_conflict_resolver(&self, resolver: Arc<dyn ConflictResolver>) -> Self {
        let mut engine = self.clone();
        engine.resolver = Some(super::ResolverSlot(resolver));
        engine
    }

    /// A copy of these engines that keeps (`true`) or drops modification
    /// times (see [`crate::settings::DataIntegritySettings::preserve_mtime`]).
    #[must_use]
    pub fn with_preserve_mtime(&self, preserve: bool) -> Self {
        let mut engine = self.clone();
        engine.options.preserve_mtime = preserve;
        engine
    }

    /// Applies the policy to the upload of the local file `path` to `dst`
    /// (for callers that transform the file before they hand it to
    /// [`TransferEngine::upload`], such as client-side encryption).
    pub(crate) async fn place_upload_path(
        &self,
        path: &Path,
        dst: &S3Uri,
        hooks: &Hooks,
    ) -> Result<Placed<S3Uri>> {
        if self.options.if_exists == IfExists::Overwrite || hooks.stop.reason().is_some() {
            return Ok(Placed::Go(dst.clone()));
        }
        let info = local_info(path).await?.unwrap_or_default();
        self.place_remote(
            &self.s3,
            TransferKind::Upload,
            path.display().to_string(),
            info,
            dst,
            hooks,
        )
        .await
    }

    /// Applies the policy to a local destination.
    pub(super) async fn place_local(
        &self,
        kind: TransferKind,
        source: String,
        source_info: ItemInfo,
        dest: &Path,
        hooks: &Hooks,
    ) -> Result<Placed<PathBuf>> {
        let probe = |path: PathBuf| async move { local_info(&path).await };
        let name = |n: u32| renamed_path(dest, n);
        self.place(
            kind,
            source,
            source_info,
            dest.display().to_string(),
            dest.to_path_buf(),
            hooks,
            probe,
            name,
        )
        .await
    }

    /// Applies the policy to an object destination reached through `s3`.
    pub(super) async fn place_remote(
        &self,
        s3: &S3,
        kind: TransferKind,
        source: String,
        source_info: ItemInfo,
        dst: &S3Uri,
        hooks: &Hooks,
    ) -> Result<Placed<S3Uri>> {
        let probe = |uri: S3Uri| {
            let s3 = s3.clone();
            async move { remote_info(&s3, &uri).await }
        };
        let name = |n: u32| renamed_uri(dst, n);
        self.place(
            kind,
            source,
            source_info,
            dst.to_string(),
            dst.clone(),
            hooks,
            probe,
            name,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn place<D, P, Fut>(
        &self,
        kind: TransferKind,
        source: String,
        source_info: ItemInfo,
        shown: String,
        target: D,
        hooks: &Hooks,
        probe: P,
        name: impl Fn(u32) -> Result<D>,
    ) -> Result<Placed<D>>
    where
        D: Clone,
        P: Fn(D) -> Fut,
        Fut: std::future::Future<Output = Result<Option<ItemInfo>>>,
    {
        let policy = self.options.if_exists;
        if policy == IfExists::Overwrite {
            return Ok(Placed::Go(target));
        }
        let Some(existing) = probe(target.clone()).await? else {
            return Ok(Placed::Go(target));
        };
        let choice = match policy {
            IfExists::Ask => {
                let Some(resolver) = &self.resolver else {
                    return Err(Error::Usage(format!(
                        "{shown} already exists; choose what to do with --if-exists or the overwrite policy setting"
                    )));
                };
                let conflict = Conflict {
                    kind,
                    source,
                    destination: shown.clone(),
                    source_info: source_info.clone(),
                    destination_info: existing.clone(),
                };
                tokio::select! {
                    biased;
                    () = hooks.stop.stopped() => return Ok(Placed::Stop(hooks.stop.reason())),
                    choice = resolver.0.resolve(&conflict) => choice?,
                }
            }
            other => Choice::try_from(other)?,
        };
        match choice {
            Choice::Overwrite => Ok(Placed::Go(target)),
            Choice::Skip => Ok(Placed::Skip(SkipReason::Exists)),
            Choice::OverwriteIfNewer => {
                if is_newer(source_info.modified, existing.modified) {
                    Ok(Placed::Go(target))
                } else {
                    Ok(Placed::Skip(SkipReason::NotNewer))
                }
            }
            Choice::Rename => {
                for n in 1..=MAX_RENAME_TRIES {
                    let candidate = name(n)?;
                    if probe(candidate.clone()).await?.is_none() {
                        return Ok(Placed::Go(candidate));
                    }
                }
                Err(Error::Conflict(format!(
                    "no free name for {shown} after {MAX_RENAME_TRIES} tries"
                )))
            }
        }
    }
}

/// True when `source` is later than `destination`, compared in whole
/// seconds (S3 keeps no finer time). An unknown time is never newer.
pub fn is_newer(source: Option<DateTime<Utc>>, destination: Option<DateTime<Utc>>) -> bool {
    match (source, destination) {
        (Some(source), Some(destination)) => source.timestamp() > destination.timestamp(),
        _ => false,
    }
}

async fn local_info(path: &Path) -> Result<Option<ItemInfo>> {
    match tokio::fs::metadata(path).await {
        Ok(meta) => Ok(Some(ItemInfo {
            size: Some(meta.len()),
            modified: meta.modified().ok().map(DateTime::<Utc>::from),
        })),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(Error::io(
            format!("checking whether '{}' exists", path.display()),
            err,
        )),
    }
}

async fn remote_info(s3: &S3, uri: &S3Uri) -> Result<Option<ItemInfo>> {
    match s3.head_object(uri.bucket(), uri.key(), None).await {
        Ok(head) => Ok(Some(ItemInfo {
            size: Some(head.size),
            modified: object_time(&head),
        })),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// The time of an object for comparisons: the `mtime` user metadata an
/// upload stored, else `Last-Modified`.
pub fn object_time(head: &ObjectHead) -> Option<DateTime<Utc>> {
    head.meta
        .user_meta
        .get(MTIME_META_KEY)
        .and_then(|text| parse_mtime(text))
        .or(head.last_modified)
}

/// The text stored in the `mtime` user metadata: Unix seconds, with a
/// fraction when the time has one (`1700000000`, `1700000000.5`).
pub fn format_mtime(time: DateTime<Utc>) -> String {
    let nanos = time.timestamp_subsec_nanos();
    if nanos == 0 {
        time.timestamp().to_string()
    } else {
        let fraction = format!("{nanos:09}");
        format!("{}.{}", time.timestamp(), fraction.trim_end_matches('0'))
    }
}

/// Reads the `mtime` user metadata: Unix seconds with an optional fraction,
/// or an RFC 3339 time. `None` for anything else.
pub fn parse_mtime(text: &str) -> Option<DateTime<Utc>> {
    let text = text.trim();
    let (whole, fraction) = match text.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (text, None),
    };
    if let Ok(secs) = whole.parse::<i64>() {
        let nanos = match fraction {
            None => 0,
            Some(digits) if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) => {
                let digits = &digits[..digits.len().min(9)];
                digits.parse::<u32>().ok()? * 10u32.pow(9 - u32::try_from(digits.len()).ok()?)
            }
            Some(_) => return None,
        };
        return DateTime::from_timestamp(secs, nanos);
    }
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

/// `name (n).ext`; a name without a real extension gets ` (n)` at the end.
pub fn numbered_name(name: &str, n: u32) -> String {
    match name.rfind('.') {
        Some(dot) if dot > 0 && dot + 1 < name.len() => {
            format!("{} ({n}){}", &name[..dot], &name[dot..])
        }
        _ => format!("{name} ({n})"),
    }
}

fn renamed_path(dest: &Path, n: u32) -> Result<PathBuf> {
    let name = dest
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::Usage(format!("'{}' has no usable file name", dest.display())))?;
    Ok(dest.with_file_name(numbered_name(name, n)))
}

fn renamed_uri(dst: &S3Uri, n: u32) -> Result<S3Uri> {
    let key = dst.key();
    let (parent, name) = match key.rfind('/') {
        Some(slash) => key.split_at(slash + 1),
        None => ("", key),
    };
    S3Uri::new(dst.bucket(), format!("{parent}{}", numbered_name(name, n)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_names() {
        assert_eq!(numbered_name("a.txt", 1), "a (1).txt");
        assert_eq!(numbered_name("a.tar.gz", 2), "a.tar (2).gz");
        assert_eq!(numbered_name("noext", 3), "noext (3)");
        assert_eq!(numbered_name(".hidden", 1), ".hidden (1)");
        assert_eq!(numbered_name("trailing.", 1), "trailing. (1)");
        let uri = S3Uri::new("b", "dir/sub/a.txt").unwrap();
        assert_eq!(renamed_uri(&uri, 4).unwrap().key(), "dir/sub/a (4).txt");
        let top = S3Uri::new("b", "a.txt").unwrap();
        assert_eq!(renamed_uri(&top, 1).unwrap().key(), "a (1).txt");
        assert_eq!(
            renamed_path(Path::new("/x/y/a.txt"), 2).unwrap(),
            PathBuf::from("/x/y/a (2).txt")
        );
    }

    #[test]
    fn mtime_text_round_trips() {
        let whole = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        assert_eq!(format_mtime(whole), "1700000000");
        let fractional = DateTime::from_timestamp(1_700_000_000, 123_000_000).unwrap();
        assert_eq!(format_mtime(fractional), "1700000000.123");
        for time in [whole, fractional] {
            assert_eq!(parse_mtime(&format_mtime(time)), Some(time));
        }
        assert_eq!(parse_mtime("2023-11-14T22:13:20Z"), Some(whole));
        assert_eq!(parse_mtime(" 1700000000 "), Some(whole));
        assert_eq!(parse_mtime("soon"), None);
        assert_eq!(parse_mtime("12.x"), None);
        assert_eq!(parse_mtime(""), None);
    }

    #[test]
    fn newer_compares_whole_seconds_and_distrusts_unknown_times() {
        let at = |secs, nanos| DateTime::from_timestamp(secs, nanos);
        assert!(is_newer(at(11, 0), at(10, 0)));
        assert!(!is_newer(at(10, 900), at(10, 0)));
        assert!(!is_newer(at(9, 0), at(10, 0)));
        assert!(!is_newer(None, at(10, 0)));
        assert!(!is_newer(at(10, 0), None));
    }

    #[test]
    fn choices_map_to_policies() {
        for choice in [
            Choice::Overwrite,
            Choice::Skip,
            Choice::OverwriteIfNewer,
            Choice::Rename,
        ] {
            assert_eq!(Choice::try_from(IfExists::from(choice)).unwrap(), choice);
        }
        assert!(Choice::try_from(IfExists::Ask).is_err());
        assert_eq!(
            "newer".parse::<IfExists>().unwrap(),
            IfExists::OverwriteIfNewer
        );
        assert!("sometimes".parse::<IfExists>().is_err());
    }
}
