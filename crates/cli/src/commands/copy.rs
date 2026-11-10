//! `bb cp` and `bb mv`: uploads, downloads and copies between S3
//! locations, one transfer engine run per file or object.
//!
//! Every transfer of an invocation shares the bandwidth limit of the
//! settings; at most `max_concurrent_tasks` run at a time, and each is tried
//! again `retry_attempts` times after a retryable failure (a multipart
//! upload or a partial download continues where it stopped). `mv` deletes
//! each source only after its own transfer succeeded. Ctrl-C stops the
//! transfers: the engines abort unfinished multipart uploads and remove
//! partial downloads.

use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::pin::pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use bucketbridge_core::cse::{self, Compression};
use bucketbridge_core::engine::{
    CopyItem, DownloadState, ExpandFailure, Expansion, Hooks, Outcome, SkipReason, StopSignal,
    Throttle, TransferEngine, TransferOptions, UploadState, expand_upload, verify_copy,
};
use bucketbridge_core::filter::IncludeExclude;
use bucketbridge_core::path::join_key;
use bucketbridge_core::secrets::SecretString;
use bucketbridge_core::settings::IfExists;
use bucketbridge_core::transfer::{RetryPolicy, SplitMix64};
use bucketbridge_core::{BbError, Error, ErrorKind, Result, S3, S3Uri};
use futures::{StreamExt, TryStreamExt, stream};

use crate::ask::TerminalResolver;
use crate::cli::{Command, CopyArgs, GlobalArgs};
use crate::context::{Context, Inputs, account_s3};
use crate::i18n::trf;
use crate::location::{
    Location, check_recursive_pair, relative_to_source, single_destination, upload_prefix,
};
use crate::output::{Batch, Failure, Output, error_of_kind};
use crate::paging::SeenTokens;
use crate::records::CopyRecord;
use crate::transfer::{ItemProgress, ProgressLine, Totals, print_above, throttle};

/// Keys per page when listing the top level of a bucket.
const PAGE_SIZE: u32 = 1000;

/// Copy, or copy and then delete the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `bb cp`
    Copy,
    /// `bb mv`
    Move,
}

/// Which way the data goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    /// Local to S3.
    Upload,
    /// S3 to local.
    Download,
    /// S3 to S3, on the server.
    Copy,
}

impl Direction {
    /// What the items of a batch are called.
    fn noun(self) -> &'static str {
        match self {
            Direction::Upload => "files",
            Direction::Download | Direction::Copy => "objects",
        }
    }
}

impl Mode {
    /// The infinitive of the operation, for building the error
    /// templates: `copy`, `move`, `upload`, `download`.
    fn infinitive(self, direction: Direction) -> &'static str {
        match (self, direction) {
            (Mode::Move, _) => "move",
            (Mode::Copy, Direction::Upload) => "upload",
            (Mode::Copy, Direction::Download) => "download",
            (Mode::Copy, Direction::Copy) => "copy",
        }
    }

    fn past(self, direction: Direction) -> &'static str {
        match (self, direction) {
            (Mode::Move, _) => "moved",
            (Mode::Copy, Direction::Upload) => "uploaded",
            (Mode::Copy, Direction::Download) => "downloaded",
            (Mode::Copy, Direction::Copy) => "copied",
        }
    }
}

/// The error templates are registered per verb, because the Chinese
/// sentence puts the verb before the object.
fn prefix_template(verb: &str, name: &str) -> String {
    trf(
        "{prefix} is a prefix; add --recursive to {verb} everything under it",
        &[("prefix", name), ("verb", verb)],
    )
}

fn onto_itself(verb: &str, source: &str) -> String {
    let template = match verb {
        "upload" => "cannot upload {source} onto itself",
        "download" => "cannot download {source} onto itself",
        "copy" => "cannot copy {source} onto itself",
        _ => "cannot move {source} onto itself",
    };
    trf(template, &[("source", source)])
}

/// Refuses a folder source without `--recursive`; the templates are
/// registered per verb (`upload`/`move`), because the Chinese sentence
/// puts the verb before the object.
fn needs_recursive_folder(verb: &str, path: &str) -> String {
    let template = match verb {
        "move" => "'{path}' is a folder; add --recursive to move it",
        _ => "'{path}' is a folder; add --recursive to upload it",
    };
    trf(template, &[("path", path)])
}

/// Reports an expansion conflict: `nothing was <past>`, per verb.
fn cannot_template(verb: &str, source: &str, message: &str, more: &str, past: &str) -> String {
    let full = match verb {
        "upload" => "cannot upload {source}: {message}{more}; nothing was uploaded",
        "download" => "cannot download {source}: {message}{more}; nothing was downloaded",
        "copy" => "cannot copy {source}: {message}{more}; nothing was copied",
        _ => "cannot move {source}: {message}{more}; nothing was moved",
    };
    trf(
        full,
        &[
            ("source", source),
            ("message", message),
            ("more", more),
            ("verb", verb),
            ("past", past),
        ],
    )
}

/// The two locations of an invocation.
#[derive(Debug, Clone, Copy)]
enum Route<'a> {
    Upload { path: &'a Path, dst: &'a S3Uri },
    Download { src: &'a S3Uri, path: &'a Path },
    Copy { src: &'a S3Uri, dst: &'a S3Uri },
}

impl Route<'_> {
    fn direction(self) -> Direction {
        match self {
            Route::Upload { .. } => Direction::Upload,
            Route::Download { .. } => Direction::Download,
            Route::Copy { .. } => Direction::Copy,
        }
    }
}

fn route(args: &CopyArgs) -> Result<Route<'_>> {
    match (&args.source, &args.destination) {
        (Location::Local(path), Location::S3(dst)) => Ok(Route::Upload { path, dst }),
        (Location::S3(src), Location::Local(path)) => Ok(Route::Download { src, path }),
        (Location::S3(src), Location::S3(dst)) => Ok(Route::Copy { src, dst }),
        (Location::Local(_), Location::Local(_)) => Err(Error::Usage(crate::i18n::tr(
            "both locations are local paths; bb uploads files to S3, downloads objects from S3 and copies objects between S3 locations",
        ))),
    }
}

/// Checks what can be checked without the file system or the network, so
/// that these usage errors come before credentials are needed.
pub fn check(args: &CopyArgs, mode: Mode) -> Result<()> {
    let route = route(args)?;
    let direction = route.direction();
    check_cse(args, direction)?;
    match direction {
        Direction::Upload if args.from_account.is_some() => {
            return Err(Error::Usage(crate::i18n::tr(
                "--from-account names the account of an S3 source; the source here is a local path",
            )));
        }
        Direction::Download if args.to_account.is_some() => {
            return Err(Error::Usage(crate::i18n::tr(
                "--to-account names the account of an S3 destination; the destination here is a local path",
            )));
        }
        _ => {}
    }
    let src = match route {
        Route::Upload { .. } => return Ok(()),
        Route::Download { src, .. } | Route::Copy { src, .. } => src,
    };
    if args.recursive && !src.is_prefix() {
        return Err(Error::Usage(trf(
            "--recursive needs a source prefix ending in '/', for example {source}/",
            &[("source", &format!("{src}/"))],
        )));
    }
    if !args.recursive && src.is_prefix() {
        return Err(Error::Usage(prefix_template(
            mode.infinitive(direction),
            &src.to_string(),
        )));
    }
    if let Route::Copy { src, dst } = route {
        if args.recursive {
            check_recursive_pair(src, dst)?;
        } else if single_destination(src, dst)? == *src {
            return Err(Error::Usage(onto_itself(
                mode.infinitive(direction),
                &src.to_string(),
            )));
        }
    }
    Ok(())
}

/// The encryption options that fit the direction of the transfer.
fn check_cse(args: &CopyArgs, direction: Direction) -> Result<()> {
    let cse = &args.cse;
    if cse.password_file.is_some() && !(cse.encrypt || cse.decrypt) {
        return Err(Error::Usage(crate::i18n::tr(
            "--password-file needs --encrypt or --decrypt",
        )));
    }
    match direction {
        Direction::Upload if cse.decrypt => Err(Error::Usage(crate::i18n::tr(
            "--decrypt applies to downloads; to decrypt an object in S3 use bb decrypt",
        ))),
        Direction::Download if cse.encrypt => Err(Error::Usage(crate::i18n::tr(
            "--encrypt applies to uploads; to encrypt an object in S3 use bb encrypt",
        ))),
        Direction::Copy if cse.encrypt || cse.decrypt => Err(Error::Usage(crate::i18n::tr(
            "--encrypt and --decrypt do not apply to copies between S3 locations; use bb encrypt or bb decrypt",
        ))),
        _ => Ok(()),
    }
}

/// What the transfers of an invocation do to the data.
enum CseOp {
    Encrypt {
        password: SecretString,
        compression: Compression,
    },
    Decrypt {
        password: SecretString,
    },
}

impl CseOp {
    fn of(args: &CopyArgs, password: Option<SecretString>) -> Result<Option<CseOp>> {
        if !(args.cse.encrypt || args.cse.decrypt) || args.dry_run {
            return Ok(None);
        }
        let password = password
            .ok_or_else(|| Error::Internal("the encryption password was not read".into()))?;
        Ok(Some(if args.cse.encrypt {
            CseOp::Encrypt {
                password,
                compression: args.cse.compress.unwrap_or(Compression::None),
            }
        } else {
            CseOp::Decrypt { password }
        }))
    }
}

/// Refuses a local file that already holds an encrypted envelope.
async fn refuse_encrypted(path: &Path) -> Result<()> {
    let file = path.to_path_buf();
    let head = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
        use std::io::Read;
        let mut head = Vec::with_capacity(cse::HEADER_LEN);
        std::fs::File::open(&file)?
            .take(cse::HEADER_LEN as u64)
            .read_to_end(&mut head)?;
        Ok(head)
    })
    .await
    .map_err(|err| Error::Internal(format!("a background task failed: {err}")))?
    .map_err(|err| {
        Error::io(
            trf("reading '{path}'", &[("path", &path.display().to_string())]),
            err,
        )
    })?;
    if cse::inspect(&head)?.is_some() {
        return Err(Error::Conflict(trf(
            "'{path}' is already client-side encrypted; it is not encrypted a second time",
            &[("path", &path.display().to_string())],
        )));
    }
    Ok(())
}

/// Runs `bb cp` or `bb mv`.
pub async fn run(
    ctx: &Context,
    args: &CopyArgs,
    rules: &IncludeExclude,
    mode: Mode,
    password: Option<SecretString>,
    accounts: &CopyAccounts,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    check(args, mode)?;
    let cse = CseOp::of(args, password)?;
    let route = route(args)?;
    let direction = route.direction();
    let policy = args.if_exists.unwrap_or(ctx.settings.transfer.if_exists);
    if policy == IfExists::Ask && !ctx.can_ask && !args.dry_run {
        return Err(Error::Usage(crate::i18n::tr(
            "the overwrite policy is 'ask', which needs a terminal; pass --if-exists overwrite, skip, newer or rename",
        ))
        .into());
    }
    // The upload goes to the destination account, everything else starts at
    // the source account; a copy between S3 locations may end in another one.
    let (primary, other) = match direction {
        Direction::Upload => (accounts.to.as_ref().unwrap_or(&ctx.s3), None),
        Direction::Download => (accounts.from.as_ref().unwrap_or(&ctx.s3), None),
        Direction::Copy => (
            accounts.from.as_ref().unwrap_or(&ctx.s3),
            (accounts.from.is_some() || accounts.to.is_some())
                .then(|| accounts.to.as_ref().unwrap_or(&ctx.s3)),
        ),
    };
    let mut options = TransferOptions::from_settings(&ctx.settings);
    options.if_exists = policy;
    options.preserve_mtime = options.preserve_mtime && !args.no_preserve_mtime;
    let resolver: Option<Arc<dyn bucketbridge_core::engine::ConflictResolver>> =
        (policy == IfExists::Ask).then(|| {
            Arc::new(TerminalResolver::new())
                as Arc<dyn bucketbridge_core::engine::ConflictResolver>
        });
    let build = |s3: &S3| -> Result<TransferEngine> {
        let engine = TransferEngine::new(s3.clone(), options.clone())?;
        let engine = if args.no_upload_rules {
            engine
        } else {
            engine.with_upload_rule_settings(&ctx.settings.upload_rules)?
        };
        Ok(match &resolver {
            Some(resolver) => engine.with_conflict_resolver(Arc::clone(resolver)),
            None => engine,
        })
    };
    let engine = build(primary)?;
    let dest = other.map(build).transpose()?;
    let planner = Planner {
        engine: &engine,
        rules,
        mode,
        recursive: args.recursive,
        dry_run: args.dry_run,
    };
    // Planning and dry runs change nothing: a stop simply drops them.
    let plan = tokio::select! {
        plan = planner.plan(route) => plan?,
        () = ctx.stop.stopped() => return Err(Error::Cancelled.into()),
    };
    if args.dry_run {
        return tokio::select! {
            result = dry_run(ctx, plan, mode, out) => result,
            () = ctx.stop.stopped() => Err(Error::Cancelled.into()),
        };
    }
    execute(ctx, &engine, dest.as_ref(), plan, mode, cse, out).await
}

/// The transports of `--from-account` and `--to-account`.
#[derive(Default)]
pub struct CopyAccounts {
    /// The account that owns the source.
    pub from: Option<S3>,
    /// The account that owns the destination.
    pub to: Option<S3>,
}

/// Builds the transports of the accounts a `cp` or `mv` names; nothing for
/// other commands.
pub async fn accounts(
    command: &Command,
    global: &GlobalArgs,
    inputs: &Inputs,
) -> Result<CopyAccounts> {
    let (Command::Cp(args) | Command::Mv(args)) = command else {
        return Ok(CopyAccounts::default());
    };
    let open = async |selector: &Option<String>| match selector {
        Some(selector) => account_s3(selector, global, inputs).await.map(Some),
        None => Ok(None),
    };
    Ok(CopyAccounts {
        from: open(&args.from_account).await?,
        to: open(&args.to_account).await?,
    })
}

/// One transfer.
#[derive(Debug, Clone)]
enum Job {
    /// A local file to an object, or an empty local folder to a folder
    /// marker.
    Upload {
        path: PathBuf,
        dst: S3Uri,
        size: u64,
        is_folder: bool,
    },
    /// An object to a local file, or a folder (marker) to a local folder.
    Download {
        src: S3Uri,
        path: PathBuf,
        size: Option<u64>,
        is_folder: bool,
    },
    /// A server-side copy.
    Copy {
        src: S3Uri,
        dst: S3Uri,
        size: Option<u64>,
    },
}

impl Job {
    fn record(&self, dry_run: bool) -> CopyRecord {
        let (source, destination, size) = match self {
            Job::Upload {
                path, dst, size, ..
            } => (path.display().to_string(), dst.to_string(), Some(*size)),
            Job::Download {
                src, path, size, ..
            } => (src.to_string(), path.display().to_string(), *size),
            Job::Copy { src, dst, size } => (src.to_string(), dst.to_string(), *size),
        };
        CopyRecord {
            source,
            destination,
            size,
            etag: None,
            dry_run,
            copied: false,
            source_deleted: false,
            skipped: None,
            error: None,
        }
    }

    /// The destination as reported when the policy renamed it to `renamed`
    /// (a key, or a path for a download).
    fn landed(&self, renamed: &str) -> String {
        match self {
            Job::Upload { dst, .. } | Job::Copy { dst, .. } => {
                format!("s3://{}/{renamed}", dst.bucket())
            }
            Job::Download { .. } => renamed.to_owned(),
        }
    }

    /// Bytes expected before the transfer starts.
    fn expected_size(&self) -> u64 {
        match self {
            Job::Upload { size, .. } => *size,
            Job::Download { size, .. } | Job::Copy { size, .. } => size.unwrap_or(0),
        }
    }
}

/// An entry the expansion left out, reported as a failed item.
struct Skipped {
    record: CopyRecord,
    kind: ErrorKind,
    message: String,
}

/// Everything an invocation will do.
struct Plan {
    direction: Direction,
    /// One named file or object: its error ends the command (rather than
    /// being counted in a batch).
    single: bool,
    jobs: Vec<Job>,
    skipped: Vec<Skipped>,
    /// `mv` of a local folder: the folder, whose emptied subfolders are
    /// removed at the end.
    source_dir: Option<PathBuf>,
}

impl Plan {
    fn single(direction: Direction, job: Job) -> Self {
        Plan {
            direction,
            single: true,
            jobs: vec![job],
            skipped: Vec::new(),
            source_dir: None,
        }
    }
}

/// Turns the locations into jobs, following the folder semantics of the
/// transfer commands: a selected folder or prefix lands under the
/// destination with its own name.
struct Planner<'a> {
    engine: &'a TransferEngine,
    rules: &'a IncludeExclude,
    mode: Mode,
    recursive: bool,
    dry_run: bool,
}

impl Planner<'_> {
    async fn plan(&self, route: Route<'_>) -> Result<Plan> {
        match route {
            Route::Upload { path, dst } if self.recursive => self.upload_folder(path, dst).await,
            Route::Upload { path, dst } => self.upload_file(path, dst).await,
            Route::Download { src, path } if self.recursive => {
                self.download_prefix(src, path).await
            }
            Route::Download { src, path } => self.download_object(src, path).await,
            Route::Copy { src, dst } if self.recursive => self.copy_prefix(src, dst).await,
            Route::Copy { src, dst } => Ok(Plan::single(
                Direction::Copy,
                Job::Copy {
                    src: src.clone(),
                    dst: single_destination(src, dst)?,
                    size: None,
                },
            )),
        }
    }

    /// Whether the filters keep an entry; `relative` is its path below the
    /// selected folder or prefix (empty for the selection itself, which is
    /// always kept).
    fn keep(&self, relative: &str) -> bool {
        relative.is_empty() || self.rules.is_included(relative)
    }

    /// One file: to the exact key, or under a prefix with its own name.
    async fn upload_file(&self, path: &Path, dst: &S3Uri) -> Result<Plan> {
        let metadata = local_metadata(path).await?;
        if metadata.is_dir() {
            return Err(Error::Usage(needs_recursive_folder(
                self.mode.infinitive(Direction::Upload),
                &path.display().to_string(),
            )));
        }
        let key = if dst.is_prefix() {
            join_key(dst.key(), utf8_name(path)?)
        } else {
            dst.key().to_owned()
        };
        Ok(Plan::single(
            Direction::Upload,
            Job::Upload {
                path: path.to_path_buf(),
                dst: S3Uri::new(dst.bucket(), key)?,
                size: metadata.len(),
                is_folder: false,
            },
        ))
    }

    /// A folder (or file) with everything in it, under the destination
    /// prefix.
    async fn upload_folder(&self, path: &Path, dst: &S3Uri) -> Result<Plan> {
        let selected = named(path).await?;
        let is_dir = local_metadata(&selected).await?.is_dir();
        let name = utf8_name(&selected)?.to_owned();
        let prefix = upload_prefix(dst);
        let expansion = expand_upload(vec![selected.clone()], prefix.clone()).await?;
        let base = format!("{}/", join_key(&prefix, &name));
        let skipped = self.skipped(
            expansion.failures,
            |entry| {
                if is_dir {
                    Path::new(entry)
                        .strip_prefix(&selected)
                        .map(slash_path)
                        .unwrap_or_default()
                } else {
                    name.clone()
                }
            },
            str::to_owned,
            &dst.to_string(),
            Direction::Upload,
        )?;
        let mut jobs = Vec::with_capacity(expansion.items.len());
        for item in expansion.items {
            let relative = if is_dir {
                item.key.strip_prefix(base.as_str()).unwrap_or(&item.key)
            } else {
                name.as_str()
            };
            if !self.keep(relative) {
                continue;
            }
            jobs.push(Job::Upload {
                dst: S3Uri::new(dst.bucket(), item.key)?,
                path: item.path,
                size: item.size,
                is_folder: item.is_folder,
            });
        }
        Ok(Plan {
            direction: Direction::Upload,
            single: false,
            jobs,
            skipped,
            source_dir: (self.mode == Mode::Move && is_dir).then_some(selected),
        })
    }

    /// One object: to the exact path, or into an existing folder (or a path
    /// ending with a separator) with its own name.
    async fn download_object(&self, src: &S3Uri, path: &Path) -> Result<Plan> {
        let target = if ends_with_separator(path) || is_local_dir(path).await {
            let expansion = self
                .engine
                .expand_download(src.bucket(), &[src.key().to_owned()], path)
                .await?;
            if let Some(failure) = expansion.failures.into_iter().next() {
                return Err(error_of_kind(failure.code, failure.message));
            }
            expansion
                .items
                .into_iter()
                .next()
                .map(|item| item.path)
                .ok_or_else(|| {
                    Error::Internal(trf(
                        "no download was planned for {source}",
                        &[("source", &src.to_string())],
                    ))
                })?
        } else {
            path.to_path_buf()
        };
        Ok(Plan::single(
            Direction::Download,
            Job::Download {
                src: src.clone(),
                path: target,
                size: None,
                is_folder: false,
            },
        ))
    }

    /// Everything under a prefix, into a local folder.
    async fn download_prefix(&self, src: &S3Uri, path: &Path) -> Result<Plan> {
        if let Ok(metadata) = tokio::fs::metadata(path).await
            && !metadata.is_dir()
        {
            return Err(Error::Usage(trf(
                if self.mode == Mode::Move {
                    "'{path}' is a file; a recursive move needs a folder to put the objects in"
                } else {
                    "'{path}' is a file; a recursive download needs a folder to put the objects in"
                },
                &[("path", &path.display().to_string())],
            )));
        }
        let keys = if src.is_bucket_root() {
            top_level_keys(self.engine.s3(), src.bucket()).await?
        } else {
            vec![src.key().to_owned()]
        };
        let expansion = self
            .engine
            .expand_download(src.bucket(), &keys, path)
            .await?;
        let mut items = expansion.items;
        // The selected folder itself is always planned; when nothing is
        // stored under it, not even a folder marker, there is nothing to do.
        if !src.is_bucket_root() && items.len() == 1 && !self.folder_exists(src).await? {
            items.clear();
        }
        let skipped = self.skipped(
            expansion.failures,
            |key| relative_to_source(src, key).to_owned(),
            |key| format!("s3://{}/{key}", src.bucket()),
            &path.display().to_string(),
            Direction::Download,
        )?;
        let mut jobs = Vec::with_capacity(items.len());
        for item in items {
            if !self.keep(relative_to_source(src, &item.key)) {
                continue;
            }
            jobs.push(Job::Download {
                src: S3Uri::new(src.bucket(), item.key)?,
                path: item.path,
                size: item.size,
                is_folder: item.is_folder,
            });
        }
        Ok(Plan {
            direction: Direction::Download,
            single: false,
            jobs,
            skipped,
            source_dir: None,
        })
    }

    /// Everything under a prefix, to the destination prefix.
    async fn copy_prefix(&self, src: &S3Uri, dst: &S3Uri) -> Result<Plan> {
        let prefix = upload_prefix(dst);
        let keys = [src.key().to_owned()];
        let expansion = if src.is_bucket_root() {
            whole_bucket(self.engine.s3(), src.bucket(), &prefix).await?
        } else if self.mode == Mode::Move {
            self.engine
                .expand_move(src.bucket(), &keys, dst.bucket(), &prefix)
                .await?
        } else {
            self.engine
                .expand_copy(src.bucket(), &keys, dst.bucket(), &prefix)
                .await?
        };
        let skipped = self.skipped(
            expansion.failures,
            |key| relative_to_source(src, key).to_owned(),
            |key| format!("s3://{}/{key}", src.bucket()),
            &dst.to_string(),
            Direction::Copy,
        )?;
        let mut jobs = Vec::with_capacity(expansion.items.len());
        for item in expansion.items {
            if !self.keep(relative_to_source(src, &item.src_key)) {
                continue;
            }
            jobs.push(Job::Copy {
                src: S3Uri::new(src.bucket(), item.src_key)?,
                dst: S3Uri::new(dst.bucket(), item.dst_key)?,
                size: item.size,
            });
        }
        Ok(Plan {
            direction: Direction::Copy,
            single: false,
            jobs,
            skipped,
            source_dir: None,
        })
    }

    /// Whether the selected prefix exists as a folder marker.
    async fn folder_exists(&self, src: &S3Uri) -> Result<bool> {
        match self
            .engine
            .s3()
            .head_object(src.bucket(), src.key(), None)
            .await
        {
            Ok(_) => Ok(true),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err),
        }
    }

    /// The expansion failures the filters keep, as skipped entries. Two
    /// entries that would land on the same destination reject the whole
    /// command before anything is transferred.
    fn skipped(
        &self,
        failures: Vec<ExpandFailure>,
        relative: impl Fn(&str) -> String,
        source: impl Fn(&str) -> String,
        destination: &str,
        direction: Direction,
    ) -> Result<Vec<Skipped>> {
        let failures: Vec<ExpandFailure> = failures
            .into_iter()
            .filter(|failure| self.keep(&relative(&failure.entry)))
            .collect();
        let mut conflicts = failures
            .iter()
            .filter(|failure| failure.code == ErrorKind::Conflict);
        if let Some(first) = conflicts.next() {
            let more = match conflicts.count() {
                0 => String::new(),
                more => trf(" (and {more} more)", &[("more", more.to_string().as_str())]),
            };
            let verb = self.mode.infinitive(direction);
            let past = self.mode.past(direction);
            return Err(Error::Conflict(cannot_template(
                verb,
                &source(&first.entry),
                &first.message,
                &more,
                past,
            )));
        }
        Ok(failures
            .into_iter()
            .map(|failure| Skipped {
                record: CopyRecord {
                    source: source(&failure.entry),
                    destination: destination.to_owned(),
                    size: None,
                    etag: None,
                    dry_run: self.dry_run,
                    copied: false,
                    source_deleted: false,
                    skipped: None,
                    error: Some(BbError {
                        code: failure.code,
                        message: failure.message.clone(),
                    }),
                },
                kind: failure.code,
                message: failure.message,
            })
            .collect())
    }
}

/// Metadata of a local source; a missing path is an `io` error.
async fn local_metadata(path: &Path) -> Result<std::fs::Metadata> {
    tokio::fs::metadata(path).await.map_err(|err| {
        Error::io(
            trf("reading '{path}'", &[("path", &path.display().to_string())]),
            err,
        )
    })
}

async fn is_local_dir(path: &Path) -> bool {
    tokio::fs::metadata(path)
        .await
        .is_ok_and(|metadata| metadata.is_dir())
}

/// Whether a path was written with a trailing separator (`out/`).
fn ends_with_separator(path: &Path) -> bool {
    path.as_os_str()
        .to_string_lossy()
        .ends_with(std::path::is_separator)
}

/// The last component of a path, which names it in S3.
fn utf8_name(path: &Path) -> Result<&str> {
    path.file_name().and_then(OsStr::to_str).ok_or_else(|| {
        Error::Usage(trf(
            "'{path}' has no name that can be used in an S3 key (it must end with a valid UTF-8 name)",
            &[("path", &path.display().to_string())],
        ))
    })
}

/// A selected path with a name: `.` and `..` are resolved to the folder
/// they stand for.
async fn named(path: &Path) -> Result<PathBuf> {
    if path.file_name().is_some() {
        return Ok(path.to_path_buf());
    }
    let resolved = tokio::fs::canonicalize(path).await.map_err(|err| {
        Error::io(
            trf("reading '{path}'", &[("path", &path.display().to_string())]),
            err,
        )
    })?;
    utf8_name(&resolved)?;
    Ok(resolved)
}

/// A relative local path with `/` separators, as filters see it.
fn slash_path(relative: &Path) -> String {
    relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// The keys and prefixes at the top of a bucket, so that downloading a
/// whole bucket puts its content directly into the folder.
async fn top_level_keys(s3: &S3, bucket: &str) -> Result<Vec<String>> {
    let mut keys = Vec::new();
    let mut seen = SeenTokens::default();
    let mut token: Option<String> = None;
    loop {
        let page = s3
            .list_objects(bucket, "", false, token.as_deref(), PAGE_SIZE)
            .await?;
        keys.extend(page.entries.into_iter().map(|entry| entry.key));
        match page.next_token {
            Some(next) => {
                let what = trf("listing s3://{bucket}/", &[("bucket", bucket)]);
                token = Some(seen.follow(next, &what)?)
            }
            None => return Ok(keys),
        }
    }
}

/// Every object of a bucket, each to `prefix` + its key.
async fn whole_bucket(s3: &S3, bucket: &str, prefix: &str) -> Result<Expansion<CopyItem>> {
    let items = s3
        .list_all_stream(bucket, "")
        .map_ok(|entry| CopyItem {
            dst_key: join_key(prefix, &entry.key),
            src_key: entry.key,
            size: Some(entry.size),
        })
        .try_collect()
        .await?;
    Ok(Expansion {
        items,
        failures: Vec::new(),
    })
}

/// `--dry-run`: the planned records, nothing transferred or deleted.
async fn dry_run(
    ctx: &Context,
    mut plan: Plan,
    mode: Mode,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    if plan.single {
        // The named object must exist, as for the real transfer.
        for job in &mut plan.jobs {
            if let Job::Download { src, size, .. } | Job::Copy { src, size, .. } = job {
                *size = Some(
                    ctx.s3
                        .head_object(src.bucket(), src.key(), None)
                        .await?
                        .size,
                );
            }
        }
    }
    let mut report = Report::new(out, None, mode, plan.direction);
    for skipped in &plan.skipped {
        report.skipped(skipped);
    }
    for job in &plan.jobs {
        report.done(&job.record(true));
    }
    report.finish()
}

/// Runs the jobs and reports each as it finishes (in plan order).
async fn execute(
    ctx: &Context,
    engine: &TransferEngine,
    dest: Option<&TransferEngine>,
    plan: Plan,
    mode: Mode,
    cse: Option<CseOp>,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    let Plan {
        direction,
        single,
        jobs,
        skipped,
        source_dir,
    } = plan;
    let totals = Totals::new(jobs.len(), jobs.iter().map(Job::expected_size).sum());
    let line = ctx
        .show_progress
        .then(|| ProgressLine::start(Arc::clone(&totals), out.stderr().clone()));
    let runner = Runner {
        engine,
        dest,
        mode,
        direction,
        policy: RetryPolicy::from_settings(&ctx.settings.transfer),
        throttle: throttle(&ctx.settings.transfer),
        stop: ctx.stop.clone(),
        totals,
        cse,
    };
    let mut report = Report::new(out, line, mode, direction);
    for entry in &skipped {
        report.skipped(entry);
    }
    let concurrency = usize::try_from(ctx.settings.transfer.max_concurrent_tasks)
        .unwrap_or(1)
        .max(1);
    let mut moved_files = Vec::new();
    let mut single_error = None;
    let mut results = pin!(
        stream::iter(jobs)
            .map(|job| runner.run(job))
            .buffered(concurrency)
    );
    while let Some((job, ended)) = results.next().await {
        match ended {
            Ended::Done(record) => {
                if mode == Mode::Move
                    && let Job::Upload { path, .. } = job
                {
                    moved_files.push(path);
                }
                report.done(&record);
            }
            Ended::Skipped(record) => report.skipped_item(&record),
            Ended::Failed(record, err) => {
                // A single item's error ends the command; its record is only
                // needed when part of it happened (mv: copied, not deleted).
                if !single || record.copied {
                    report.failed(&record, &err);
                }
                if single {
                    single_error = Some(err);
                }
            }
            Ended::Stopped => {}
        }
        if report.broken.is_some() {
            // Nobody reads the results any more: stop, letting the engines
            // clean up.
            ctx.stop.cancel();
        }
    }
    let stopped = ctx.stop.is_stopped();
    let closed = report.close();
    if let Some(root) = source_dir {
        // Also after a stop: what was moved is gone from its folders.
        remove_emptied_folders(&root, &moved_files).await;
    }
    let batch = closed?;
    if stopped {
        return Err(Error::Cancelled.into());
    }
    if let Some(err) = single_error {
        return Err(err.into());
    }
    batch.finish(direction.noun(), &past_failure(mode, direction))
}

/// After `mv` of a local folder: removes the folders that moving `files`
/// emptied, deepest first, up to and including `root`. Folders that still
/// hold anything (excluded files, failed transfers) stay.
async fn remove_emptied_folders(root: &Path, files: &[PathBuf]) {
    let mut folders = BTreeSet::new();
    for file in files {
        for folder in file.ancestors().skip(1) {
            if !folder.starts_with(root) {
                break;
            }
            folders.insert(folder);
        }
    }
    let mut folders: Vec<&Path> = folders.into_iter().collect();
    folders.sort_by_key(|folder| Reverse(folder.components().count()));
    for folder in folders {
        // Fails for a folder that is not empty, which is what keeps it.
        let _ = tokio::fs::remove_dir(folder).await;
    }
}

/// Prints the results, above the progress line when there is one.
struct Report<'o> {
    out: &'o mut Output,
    line: Option<ProgressLine>,
    mode: Mode,
    direction: Direction,
    batch: Batch,
    /// Writing to standard output failed: nothing more is printed.
    broken: Option<Failure>,
}

impl<'o> Report<'o> {
    fn new(
        out: &'o mut Output,
        line: Option<ProgressLine>,
        mode: Mode,
        direction: Direction,
    ) -> Self {
        Report {
            out,
            line,
            mode,
            direction,
            batch: Batch::default(),
            broken: None,
        }
    }

    fn done(&mut self, record: &CopyRecord) {
        self.batch.succeeded();
        let text = done_line(self.mode, self.direction, record);
        self.print(|out| out.change(record, || text));
    }

    fn failed(&mut self, record: &CopyRecord, err: &Error) {
        self.batch.failed(err.kind(), err.to_string());
        let text = failed_to_line(self.mode, self.direction, record, &err.to_string());
        self.print(|out| out.item_failed(record, || text));
    }

    /// An item the overwrite policy left alone: reported, and not a failure.
    fn skipped_item(&mut self, record: &CopyRecord) {
        self.batch.succeeded();
        let reason = record.skipped.clone().unwrap_or_default();
        let text = trf(
            "skipped {source} to {destination}: {reason}",
            &[
                ("source", record.source.as_str()),
                ("destination", record.destination.as_str()),
                ("reason", &reason),
            ],
        );
        self.print(|out| out.skipped_item(record, || text));
    }

    fn skipped(&mut self, skipped: &Skipped) {
        self.batch.failed(skipped.kind, skipped.message.clone());
        let verb = self.mode.infinitive(self.direction);
        // The failures are planned rejects: no `nothing was …` here.
        let text = cannot_template(
            verb,
            &skipped.record.source,
            &skipped.message,
            "",
            self.mode.past(self.direction),
        );
        self.print(|out| out.item_failed(&skipped.record, || text));
    }

    fn print(&mut self, write: impl FnOnce(&mut Output) -> std::result::Result<(), Failure>) {
        if self.broken.is_some() {
            return;
        }
        let out = &mut *self.out;
        if let Err(failure) = print_above(self.line.as_ref(), || write(out)) {
            self.broken = Some(failure);
        }
    }

    /// Clears the progress line; the counted results, or the write failure.
    fn close(self) -> std::result::Result<Batch, Failure> {
        drop(self.line);
        match self.broken {
            Some(failure) => Err(failure),
            None => Ok(self.batch),
        }
    }

    fn finish(self) -> std::result::Result<(), Failure> {
        let (mode, direction) = (self.mode, self.direction);
        self.close()?
            .finish(direction.noun(), &past_failure(mode, direction))
    }
}

/// The `could not be <past>` part of a batch summary.
fn past_failure(mode: Mode, direction: Direction) -> String {
    crate::i18n::tr(match mode.past(direction) {
        "uploaded" => "could not be uploaded",
        "downloaded" => "could not be downloaded",
        "copied" => "could not be copied",
        _ => "could not be moved",
    })
}

fn done_line(mode: Mode, direction: Direction, record: &CopyRecord) -> String {
    let template = if record.dry_run {
        match mode.infinitive(direction) {
            "upload" => "would upload {source} to {destination}",
            "download" => "would download {source} to {destination}",
            "copy" => "would copy {source} to {destination}",
            _ => "would move {source} to {destination}",
        }
    } else {
        match mode.past(direction) {
            "uploaded" => "uploaded {source} to {destination}",
            "downloaded" => "downloaded {source} to {destination}",
            "copied" => "copied {source} to {destination}",
            _ => "moved {source} to {destination}",
        }
    };
    trf(
        template,
        &[
            ("source", record.source.as_str()),
            ("destination", record.destination.as_str()),
        ],
    )
}

/// The `bb: failed to …` line of one failed transfer.
fn failed_to_line(mode: Mode, direction: Direction, record: &CopyRecord, error: &str) -> String {
    let template = match mode.infinitive(direction) {
        "upload" => "failed to upload {source} to {destination}: {error}",
        "download" => "failed to download {source} to {destination}: {error}",
        "copy" => "failed to copy {source} to {destination}: {error}",
        _ => "failed to move {source} to {destination}: {error}",
    };
    trf(
        template,
        &[
            ("source", record.source.as_str()),
            ("destination", record.destination.as_str()),
            ("error", error),
        ],
    )
}

/// How a job ended.
enum Ended {
    Done(CopyRecord),
    /// Nothing was transferred and the source is untouched: the destination
    /// exists and the overwrite policy keeps it.
    Skipped(CopyRecord),
    Failed(CopyRecord, Error),
    /// Stopped by Ctrl-C (the engine cleaned up); not reported.
    Stopped,
}

/// What a finished transfer reports.
struct Done {
    size: u64,
    etag: Option<String>,
    /// The destination when the policy renamed it (a path, or a key).
    renamed: Option<String>,
}

/// How a transfer ended when it did not fail.
enum Transferred {
    Done(Done),
    Skipped(SkipReason),
    Stopped,
}

/// Runs jobs through the engines with the shared hooks and retries.
struct Runner<'a> {
    engine: &'a TransferEngine,
    /// The engine of the destination account of a copy between accounts.
    dest: Option<&'a TransferEngine>,
    mode: Mode,
    direction: Direction,
    policy: RetryPolicy,
    throttle: Arc<dyn Throttle>,
    stop: StopSignal,
    totals: Arc<Totals>,
    cse: Option<CseOp>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Runner<'_> {
    async fn run(&self, job: Job) -> (Job, Ended) {
        if self.stop.is_stopped() {
            return (job, Ended::Stopped);
        }
        let ended = self.transfer_and_delete(&job).await;
        (job, ended)
    }

    async fn transfer_and_delete(&self, job: &Job) -> Ended {
        let mut record = job.record(false);
        let hooks = Hooks::new()
            .with_throttle(Arc::clone(&self.throttle))
            .with_progress(ItemProgress::new(
                Arc::clone(&self.totals),
                job.expected_size(),
            ))
            .with_stop(self.stop.clone());
        let transferred = self.transfer(job, &hooks).await;
        self.totals.item_finished();
        let done = match transferred {
            Ok(Transferred::Done(done)) => done,
            Ok(Transferred::Skipped(reason)) => {
                // Nothing was written; a move keeps its source.
                record.skipped = Some(reason.describe().to_owned());
                return Ended::Skipped(record);
            }
            Ok(Transferred::Stopped) => return Ended::Stopped,
            Err(_) if self.stop.is_stopped() => return Ended::Stopped,
            Err(err) => {
                record.error = Some(err.to_payload());
                return Ended::Failed(record, err);
            }
        };
        record.copied = true;
        record.size = Some(done.size);
        record.etag = done.etag.clone();
        if let Some(renamed) = &done.renamed {
            record.destination = job.landed(renamed);
        }
        if self.mode == Mode::Move {
            // Only now that this transfer succeeded, and, between accounts,
            // only after the copy was compared with its source.
            if let Err(err) = self.verify_then_remove(job, &done).await {
                let context = match self.direction {
                    Direction::Upload => "the upload succeeded but the source could not be deleted",
                    Direction::Download => {
                        "the download succeeded but the source could not be deleted"
                    }
                    Direction::Copy => "the copy succeeded but the source could not be deleted",
                };
                let err = err.with_context(crate::i18n::tr(context));
                record.error = Some(err.to_payload());
                return Ended::Failed(record, err);
            }
            record.source_deleted = true;
        }
        Ended::Done(record)
    }

    /// One transfer with retries; `None` when it was stopped. A transfer
    /// that finally fails leaves nothing behind: its multipart upload is
    /// aborted, its partial download removed.
    async fn transfer(&self, job: &Job, hooks: &Hooks) -> Result<Transferred> {
        match job {
            Job::Upload { path, dst, .. } => {
                let saved = Mutex::new(None::<UploadState>);
                let checkpoint = |state: &UploadState| *lock(&saved) = Some(state.clone());
                let result = match &self.cse {
                    Some(CseOp::Encrypt {
                        password,
                        compression,
                    }) => {
                        refuse_encrypted(path).await?;
                        self.retrying(async || {
                            let resume = lock(&saved).clone();
                            cse::upload_encrypted(
                                self.engine,
                                path,
                                dst,
                                hooks,
                                password,
                                *compression,
                                resume,
                                &checkpoint,
                            )
                            .await
                        })
                        .await
                    }
                    _ => {
                        self.retrying(async || {
                            let resume = lock(&saved).clone();
                            self.engine
                                .upload_file(path, dst, hooks, resume, &checkpoint)
                                .await
                        })
                        .await
                    }
                };
                let leftover = lock(&saved).take();
                if result.is_err()
                    && let Some(state) = leftover
                {
                    // Best effort: `bb mpu abort` frees whatever is left.
                    let _ = self.engine.discard_upload(&state).await;
                }
                Ok(settle(result?, |done| Done {
                    size: done.size,
                    etag: done.etag,
                    renamed: done.renamed_to,
                }))
            }
            Job::Download {
                src,
                path,
                is_folder,
                ..
            } => {
                let saved = Mutex::new(None::<DownloadState>);
                let checkpoint = |state: &DownloadState| *lock(&saved) = Some(state.clone());
                let result = match &self.cse {
                    Some(CseOp::Decrypt { password }) => {
                        self.retrying(async || {
                            let resume = lock(&saved).clone();
                            cse::download_decrypted(
                                self.engine,
                                src,
                                path,
                                hooks,
                                Some(password),
                                resume,
                                &checkpoint,
                            )
                            .await
                        })
                        .await
                    }
                    _ => {
                        self.retrying(async || {
                            let resume = lock(&saved).clone();
                            self.engine
                                .download_file(src, path, hooks, resume, &checkpoint)
                                .await
                        })
                        .await
                    }
                };
                if result.is_err() && !is_folder {
                    // Best effort, like the engine's own clean-up on cancel.
                    let _ = self.engine.discard_download(path).await;
                }
                Ok(settle(result?, |done| Done {
                    size: done.size,
                    etag: done.etag,
                    renamed: done.renamed_to.map(|path| path.display().to_string()),
                }))
            }
            Job::Copy { src, dst, .. } => match self.dest {
                Some(dest) => {
                    let saved = Mutex::new(None::<UploadState>);
                    let checkpoint = |state: &UploadState| *lock(&saved) = Some(state.clone());
                    let result = self
                        .retrying(async || {
                            let resume = lock(&saved).clone();
                            self.engine
                                .copy_to(dest, src, dst, hooks, resume, &checkpoint)
                                .await
                        })
                        .await;
                    let leftover = lock(&saved).take();
                    if result.is_err()
                        && let Some(state) = leftover
                    {
                        // Best effort: `bb mpu abort` frees whatever is left.
                        let _ = dest.discard_upload(&state).await;
                    }
                    Ok(settle(result?, |(done, _route)| Done {
                        size: done.size,
                        etag: done.etag,
                        renamed: done.renamed_to,
                    }))
                }
                None => {
                    let result = self
                        .retrying(async || self.engine.copy(src, dst, hooks).await)
                        .await;
                    Ok(settle(result?, |done| Done {
                        size: done.size,
                        etag: done.etag,
                        renamed: done.renamed_to,
                    }))
                }
            },
        }
    }

    /// Runs `attempt` until it succeeds, fails for good, or is stopped. A
    /// stop during the wait before a retry lets the next attempt see it, so
    /// the engine cleans up as for any cancelled transfer.
    async fn retrying<T>(
        &self,
        mut attempt: impl AsyncFnMut() -> Result<Outcome<T>>,
    ) -> Result<Outcome<T>> {
        let mut random = SplitMix64::from_entropy();
        let mut retries = 0;
        loop {
            let err = match attempt().await {
                Ok(outcome) => return Ok(outcome),
                Err(err) => err,
            };
            let delay = if self.stop.is_stopped() {
                None
            } else {
                self.policy.next_delay(&err, retries, &mut random)
            };
            let Some(delay) = delay else {
                return Err(err);
            };
            retries += 1;
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = self.stop.stopped() => {}
            }
        }
    }

    /// `mv`: deletes the source of a finished transfer. Between accounts the
    /// copy is read back and compared with its source first; any difference
    /// keeps the source.
    async fn verify_then_remove(&self, job: &Job, done: &Done) -> Result<()> {
        if let (Some(dest), Job::Copy { src, dst, .. }) = (self.dest, job)
            && !src.key().ends_with('/')
        {
            let landed = match &done.renamed {
                Some(key) => S3Uri::new(dst.bucket(), key.clone())?,
                None => dst.clone(),
            };
            let before = self
                .engine
                .s3()
                .head_object(src.bucket(), src.key(), None)
                .await?;
            let after = dest
                .s3()
                .head_object(landed.bucket(), landed.key(), None)
                .await?;
            verify_copy(&before, &after).map_err(|err| {
                err.with_context(trf(
                    "the source {source} was kept because the copy {landed} could not be verified",
                    &[
                        ("source", &src.to_string()),
                        ("landed", &landed.to_string()),
                    ],
                ))
            })?;
        }
        self.remove_source(job).await
    }

    /// `mv`: deletes the source of a finished transfer.
    async fn remove_source(&self, job: &Job) -> Result<()> {
        match job {
            Job::Upload {
                path, is_folder, ..
            } => {
                let removed = if *is_folder {
                    tokio::fs::remove_dir(path).await
                } else {
                    tokio::fs::remove_file(path).await
                };
                removed.map_err(|err| {
                    Error::io(
                        trf(
                            "deleting '{path}'",
                            &[("path", &path.display().to_string())],
                        ),
                        err,
                    )
                })
            }
            Job::Download { src, is_folder, .. } => {
                delete_object(self.engine.s3(), src, *is_folder).await
            }
            Job::Copy { src, .. } => {
                delete_object(self.engine.s3(), src, src.key().ends_with('/')).await
            }
        }
    }
}

/// The result of a finished engine run: done, skipped by the overwrite
/// policy, or stopped.
fn settle<T>(outcome: Outcome<T>, done: impl FnOnce(T) -> Done) -> Transferred {
    match outcome {
        Outcome::Completed(value) => Transferred::Done(done(value)),
        Outcome::Skipped(reason) => Transferred::Skipped(reason),
        Outcome::Paused | Outcome::Cancelled => Transferred::Stopped,
    }
}

/// Deletes a moved object. A folder need not exist as a marker object, so
/// a missing one counts as deleted.
async fn delete_object(s3: &S3, uri: &S3Uri, is_folder: bool) -> Result<()> {
    match s3.delete_object(uri.bucket(), uri.key(), None).await {
        Err(err) if is_folder && err.kind() == ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::trf;

    fn record(dry_run: bool) -> CopyRecord {
        CopyRecord {
            source: "s3://a/k".into(),
            destination: "s3://b/k".into(),
            size: Some(1),
            etag: None,
            dry_run,
            copied: !dry_run,
            source_deleted: false,
            skipped: None,
            error: None,
        }
    }

    #[test]
    fn lines() {
        // The values are data; the expectations go through the same
        // templates as the code and hold in both languages.
        for (mode, direction, dry_run, template) in [
            (
                Mode::Copy,
                Direction::Copy,
                false,
                "copied {source} to {destination}",
            ),
            (
                Mode::Move,
                Direction::Upload,
                true,
                "would move {source} to {destination}",
            ),
            (
                Mode::Copy,
                Direction::Download,
                true,
                "would download {source} to {destination}",
            ),
            (
                Mode::Copy,
                Direction::Upload,
                false,
                "uploaded {source} to {destination}",
            ),
        ] {
            assert_eq!(
                done_line(mode, direction, &record(dry_run)),
                trf(
                    template,
                    &[("source", "s3://a/k"), ("destination", "s3://b/k")]
                )
            );
        }
        assert_eq!(Direction::Upload.noun(), "files");
        assert_eq!(Mode::Move.past(Direction::Download), "moved");
    }

    #[test]
    fn paths() {
        assert!(ends_with_separator(Path::new("out/")));
        assert!(!ends_with_separator(Path::new("out")));
        assert_eq!(slash_path(Path::new("a/b/c.txt")), "a/b/c.txt");
        assert_eq!(utf8_name(Path::new("dir/file.txt")).unwrap(), "file.txt");
        assert_eq!(
            utf8_name(Path::new("..")).unwrap_err().kind(),
            ErrorKind::Usage
        );
    }

    #[tokio::test]
    async fn dot_is_resolved_to_its_folder() {
        let dot = named(Path::new(".")).await.unwrap();
        assert_eq!(
            dot,
            std::env::current_dir().unwrap().canonicalize().unwrap()
        );
        assert_eq!(named(Path::new("a/b")).await.unwrap(), Path::new("a/b"));
    }

    #[tokio::test]
    async fn only_emptied_folders_are_removed() {
        let root = tempfile::tempdir().unwrap();
        let top = root.path().join("top");
        for dir in ["a/b", "c", "keep"] {
            std::fs::create_dir_all(top.join(dir)).unwrap();
        }
        std::fs::write(top.join("keep/excluded.txt"), b"x").unwrap();
        // The files that were moved are gone already.
        let moved = [
            top.join("a/b/f.txt"),
            top.join("c/g.txt"),
            top.join("keep/h.txt"),
        ];
        remove_emptied_folders(&top, &moved).await;
        assert!(!top.join("a").exists());
        assert!(!top.join("c").exists());
        assert!(top.join("keep/excluded.txt").exists());
        // The selected folder still holds `keep`.
        assert!(top.exists());
        std::fs::remove_file(top.join("keep/excluded.txt")).unwrap();
        remove_emptied_folders(&top, &[top.join("keep/h.txt")]).await;
        assert!(!top.exists());
        assert!(root.path().exists());
    }

    #[test]
    fn jobs_describe_themselves() {
        let job = Job::Upload {
            path: PathBuf::from("dir/a.txt"),
            dst: S3Uri::parse("s3://b/p/a.txt").unwrap(),
            size: 3,
            is_folder: false,
        };
        let record = job.record(true);
        assert_eq!(record.source, Path::new("dir/a.txt").display().to_string());
        assert_eq!(record.destination, "s3://b/p/a.txt");
        assert_eq!(record.size, Some(3));
        assert!(record.dry_run && !record.copied);
        let job = Job::Download {
            src: S3Uri::parse("s3://b/k").unwrap(),
            path: PathBuf::from("k"),
            size: None,
            is_folder: false,
        };
        assert_eq!(job.expected_size(), 0);
        assert_eq!(job.record(false).size, None);
    }
}
