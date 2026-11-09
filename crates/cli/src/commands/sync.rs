//! `bb sync`: makes a local folder and an S3 prefix match.
//!
//! The prefix is listed, the folder is scanned and compared on a blocking
//! thread (the core's `sync::analyze`), and then either the plan is printed
//! (`--dry-run`, which writes nothing, not even the plan store) or it is
//! stored and run by the core's `SyncExecutor`. Every row is reported as it
//! finishes, a failing row never stops the others, and Ctrl-C cancels the run
//! through the executor so that `--resume` can finish the stored plan later.
//!
//! A plan whose rows all finished is removed from the store again; a plan
//! with failed or unfinished rows stays. Nothing is stored when there is
//! nothing to execute.
//!
//! The first location may also be an `s3://` prefix: an S3 to S3 sync copies on
//! the server. `--job NAME` runs a saved sync job (see `syncjobs`), and the
//! filter, exclusion and analysis options of `bb sync` are the job's
//! `SyncOptions`. `--summary` ends the run with a report.

use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::Arc;
use std::time::Instant;

use bucketbridge_core::config::ConfigStore;
use bucketbridge_core::engine::{Progress, TransferEngine, TransferOptions};
use bucketbridge_core::ignore::read_rules_text;
use bucketbridge_core::model::{Side, SyncAction, SyncDirection, SyncPlan, SyncRow};
use bucketbridge_core::path::join_key;
use bucketbridge_core::sync::{
    self, AnalysisRequest, ExecuteOptions, SyncEvents, SyncExecutor, SyncOptions, SyncProgress,
    SyncReport, SyncStore,
};
use bucketbridge_core::syncjobs::{self, SyncJob};
use bucketbridge_core::{Dirs, Error, S3Uri};
use tokio::sync::mpsc;

use crate::cli::{SyncArgs, SyncSource};
use crate::context::{Context, env};
use crate::i18n::{tr, trf};
use crate::output::{Failure, Output};
use crate::records::{SyncRecord, SyncRowRecord, SyncSummaryRecord};
use crate::transfer::{ItemProgress, ProgressLine, Totals, print_above};

/// What an invocation asks for.
enum Target<'a> {
    /// Analyze a folder (or an S3 prefix) against a prefix.
    Plan {
        source: &'a SyncSource,
        destination: &'a S3Uri,
        direction: SyncDirection,
        delete: bool,
    },
    /// Run a saved sync job.
    Job(&'a str),
    /// Continue a stored plan.
    Resume(&'a str),
}

fn target(args: &SyncArgs) -> Result<Target<'_>, Error> {
    match (
        args.resume.as_deref(),
        args.job.as_deref(),
        args.source.as_ref(),
        args.s3.as_ref(),
    ) {
        (Some(plan_id), None, None, None) => Ok(Target::Resume(plan_id)),
        (None, Some(job), None, None) => Ok(Target::Job(job)),
        (None, None, Some(source), Some(destination)) => {
            let direction = match source {
                SyncSource::Local(_) => args
                    .direction
                    .map_or(SyncDirection::Upload, SyncDirection::from),
                SyncSource::S3(_) => {
                    if args.direction.is_some() {
                        return Err(Error::Usage(tr(
                            "--direction does not apply to a sync between two S3 locations: it always copies from the first to the second",
                        )));
                    }
                    SyncDirection::S3ToS3
                }
            };
            if args.delete && direction == SyncDirection::Bidirectional {
                return Err(Error::Usage(tr(
                    "--delete cannot be combined with --direction bidirectional: without a record of earlier runs, a file that is missing on one side may have been deleted there or may be new on the other side",
                )));
            }
            Ok(Target::Plan {
                source,
                destination,
                direction,
                delete: args.delete,
            })
        }
        _ => Err(Error::Usage(tr(
            "bb sync needs a local folder and an S3 location, two S3 locations, --job NAME or --resume PLAN_ID",
        ))),
    }
}

/// Checks what can be checked without the network or the file system, so
/// that these usage errors come before credentials are needed.
pub fn check(args: &SyncArgs) -> Result<(), Error> {
    target(args)?;
    SyncOptions {
        include: args.include.clone(),
        exclude: args.exclude.clone(),
        ..SyncOptions::default()
    }
    .validate()
}

/// What one run syncs: the arguments or a saved job, with the options of the
/// command line applied.
struct Spec {
    direction: SyncDirection,
    local: PathBuf,
    destination: S3Uri,
    source: Option<S3Uri>,
    delete: bool,
    options: SyncOptions,
}

/// Rules from `--exclude-from` and `--exclude`, and the other flags, on top of
/// `base` (the options of a job, or the defaults). The rules of a job's own file
/// are read now so that the rules of the command line come after them.
fn apply_flags(args: &SyncArgs, mut options: SyncOptions) -> Result<SyncOptions, Error> {
    let adds_rules = !args.exclude.is_empty() || args.exclude_from.is_some();
    if adds_rules && let Some(file) = options.exclude_file.take() {
        options.exclude.extend(rule_lines(Path::new(file.trim()))?);
    }
    options.exclude.extend(args.exclude.iter().cloned());
    if let Some(file) = &args.exclude_from {
        let file = std::path::absolute(file).map_err(|err| {
            Error::Usage(trf(
                "invalid exclusion file '{file}': {error}",
                &[
                    ("file", &file.display().to_string()),
                    ("error", &err.to_string()),
                ],
            ))
        })?;
        options.exclude.extend(rule_lines(&file)?);
    }
    options.include.extend(args.include.iter().cloned());
    if args.keep_empty_folders {
        options.skip_empty_folders = false;
    }
    if args.skip_duplicates {
        options.skip_duplicate_content = true;
    }
    if args.no_cache {
        options.use_cache = false;
    }
    if let Some(parallel) = args.parallel {
        options.parallelism = parallel;
    }
    options.validate()?;
    Ok(options)
}

fn rule_lines(file: &Path) -> Result<Vec<String>, Error> {
    Ok(read_rules_text(file)?.lines().map(str::to_owned).collect())
}

fn spec_from_args(args: &SyncArgs) -> Result<Spec, Error> {
    let Target::Plan {
        source,
        destination,
        direction,
        delete,
    } = target(args)?
    else {
        return Err(Error::Internal("not a plan target".into()));
    };
    let (local, s3_source) = match source {
        SyncSource::Local(path) => (path.clone(), None),
        SyncSource::S3(uri) => (PathBuf::new(), Some(sync::as_folder(uri)?)),
    };
    Ok(Spec {
        direction,
        local,
        destination: sync::as_folder(destination)?,
        source: s3_source,
        delete,
        options: apply_flags(args, SyncOptions::default())?,
    })
}

fn spec_from_job(job: SyncJob, args: &SyncArgs) -> Result<Spec, Error> {
    let source = job.s3_source.as_deref().map(S3Uri::parse).transpose()?;
    Ok(Spec {
        direction: job.direction,
        local: PathBuf::from(&job.local_root),
        destination: S3Uri::parse(&job.s3_prefix)?,
        source,
        delete: job.delete,
        options: apply_flags(args, job.options)?,
    })
}

/// The job to run for `--job NAME` and the arguments to open the context with:
/// the job's account is the account of the run.
pub fn resolve_job(
    global: &crate::cli::GlobalArgs,
    args: &SyncArgs,
    inputs: &crate::context::Inputs,
) -> Result<Option<(SyncJob, crate::cli::GlobalArgs)>, Error> {
    let Some(name) = args.job.as_deref() else {
        return Ok(None);
    };
    let dirs = crate::context::dirs(global, &inputs.env)?.ok_or_else(|| {
        Error::Usage(format!(
            "no configuration directory for the sync jobs: set {} or pass --config-dir",
            env::CONFIG_DIR
        ))
    })?;
    let job = syncjobs::find(&ConfigStore::new(&dirs), name)?;
    if let Some(account) = &global.account
        && account != &job.account_id
    {
        return Err(Error::Usage(trf(
            "the sync job '{name}' runs with the account {account}; drop --account or use --account {account2}",
            &[
                ("name", job.name.as_str()),
                ("account", job.account_id.as_str()),
                ("account2", job.account_id.as_str()),
            ],
        )));
    }
    let mut global = global.clone();
    global.account = Some(job.account_id.clone());
    Ok(Some((job, global)))
}

/// Runs `bb sync`.
pub async fn run(
    ctx: &Context,
    args: &SyncArgs,
    job: Option<SyncJob>,
    out: &mut Output,
) -> Result<(), Failure> {
    let started = Instant::now();
    let spec = match target(args)? {
        Target::Resume(plan_id) => return resume(ctx, plan_id, args.summary, out).await,
        Target::Plan { .. } => spec_from_args(args)?,
        Target::Job(name) => {
            let job = job
                .ok_or_else(|| Error::Internal(format!("the sync job '{name}' was not loaded")))?;
            spec_from_job(job, args)?
        }
    };
    if let Some(name) = &args.save_job {
        save_job(ctx, name, &spec, out).await?;
    }
    // A real run stores and executes its plan: fail before any work
    // when it cannot.
    let real = if args.dry_run {
        None
    } else {
        Some((require_dirs(ctx)?, engine(ctx)?))
    };
    let plan = analyze(ctx, &spec, !args.dry_run).await?;
    let Some((dirs, engine)) = real else {
        return report_only(plan, true, args.summary.then_some(started), out);
    };
    if sync::row_counts(&plan.rows).total == 0 {
        // Nothing to execute: no plan is stored.
        return report_only(plan, false, args.summary.then_some(started), out);
    }
    let store = open_store(dirs).await?;
    let plan = insert_plan(&store, plan).await?;
    execute(
        ctx,
        store,
        engine,
        plan,
        args.summary.then_some(started),
        out,
    )
    .await
}

/// `--save-job NAME`: stores the setup of this run as a job (a job of that name is
/// replaced).
async fn save_job(ctx: &Context, name: &str, spec: &Spec, out: &mut Output) -> Result<(), Failure> {
    let dirs = require_dirs(ctx)?.clone();
    let local_root = if spec.direction == SyncDirection::S3ToS3 {
        String::new()
    } else {
        let absolute = std::path::absolute(&spec.local).map_err(|err| {
            Error::Usage(trf(
                "invalid local folder '{folder}': {error}",
                &[
                    ("folder", &spec.local.display().to_string()),
                    ("error", &err.to_string()),
                ],
            ))
        })?;
        absolute.display().to_string()
    };
    let mut options = spec.options.clone();
    if let Some(file) = options.exclude_file.take() {
        let absolute = std::path::absolute(file.trim())
            .map_err(|err| Error::Usage(format!("invalid exclusion file '{file}': {err}")))?;
        options.exclude_file = Some(absolute.display().to_string());
    }
    let job = SyncJob {
        id: String::new(),
        name: name.to_owned(),
        account_id: ctx.account_id.clone(),
        direction: spec.direction,
        local_root,
        s3_prefix: spec.destination.to_string(),
        s3_source: spec.source.as_ref().map(ToString::to_string),
        delete: spec.delete,
        options,
    };
    let saved = blocking(move || {
        let store = ConfigStore::new(&dirs);
        let mut job = job;
        if let Ok(existing) = syncjobs::find(&store, &job.name) {
            job.id = existing.id;
        }
        syncjobs::save(&store, job)
    })
    .await?;
    out.notice(&trf(
        "bb: saved sync job '{name}'",
        &[("name", &saved.name)],
    ));
    Ok(())
}

/// The transfer engine of the account, with the transfer settings.
fn engine(ctx: &Context) -> Result<TransferEngine, Error> {
    TransferEngine::new(
        ctx.s3.clone(),
        TransferOptions::from_settings(&ctx.settings),
    )
}

/// The directories that hold the plan store.
fn require_dirs(ctx: &Context) -> Result<&Dirs, Error> {
    ctx.dirs.as_ref().ok_or_else(|| {
        Error::Usage(trf(
            "no configuration directory for the sync plans: set {env} or pass --config-dir",
            &[("env", env::CONFIG_DIR)],
        ))
    })
}

/// Runs blocking work: the plan store reads and writes files.
async fn blocking<T, F>(work: F) -> Result<T, Error>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, Error> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| Error::Internal(format!("a sync worker thread failed: {err}")))?
}

async fn open_store(dirs: &Dirs) -> Result<Arc<SyncStore>, Error> {
    let dirs = dirs.clone();
    blocking(move || SyncStore::open_in(&dirs).map(Arc::new)).await
}

async fn insert_plan(store: &Arc<SyncStore>, plan: SyncPlan) -> Result<SyncPlan, Error> {
    let store = Arc::clone(store);
    blocking(move || {
        store.insert(&plan)?;
        Ok(plan)
    })
    .await
}

/// Lists the prefix and compares it with the folder. Nothing is written; a
/// stop simply drops the work.
/// A dry run writes nothing, so it never writes the listing cache either.
async fn analyze(ctx: &Context, spec: &Spec, use_cache: bool) -> Result<SyncPlan, Error> {
    let request = AnalysisRequest {
        account_id: ctx.account_id.clone(),
        direction: spec.direction,
        local_root: spec.local.clone(),
        s3_prefix: spec.destination.clone(),
        source_prefix: spec.source.clone(),
        delete: spec.delete,
        options: spec.options.clone(),
        cache_dir: ctx
            .dirs
            .as_ref()
            .filter(|_| use_cache)
            .map(|dirs| dirs.data_dir.join(sync::listing::CACHE_DIR_NAME)),
    };
    tokio::select! {
        plan = sync::analyze_async(&ctx.s3, &request) => plan,
        () = ctx.stop.stopped() => Err(Error::Cancelled),
    }
}

/// `--resume`: runs the rows of a stored plan that are not done.
async fn resume(
    ctx: &Context,
    plan_id: &str,
    summary: bool,
    out: &mut Output,
) -> Result<(), Failure> {
    let started = Instant::now();
    let engine = engine(ctx)?;
    let store = open_store(require_dirs(ctx)?).await?;
    let plan = {
        let store = Arc::clone(&store);
        let plan_id = plan_id.to_owned();
        blocking(move || store.get(&plan_id)).await?
    };
    if plan.account_id != ctx.account_id {
        return Err(Error::Usage(trf(
            "plan {id} was made with account '{account}'; resume it with --account {account2}",
            &[
                ("id", plan.id.as_str()),
                ("account", plan.account_id.as_str()),
                ("account2", plan.account_id.as_str()),
            ],
        ))
        .into());
    }
    reach(ctx, &plan.s3_prefix).await?;
    execute(ctx, store, engine, plan, summary.then_some(started), out).await
}

/// Asks for one key of the plan's prefix before any row runs. A fresh plan was
/// made from a listing, which proves the same; a stored plan may meet changed
/// credentials or a deleted bucket, which should be one error (exit 3 or 2),
/// not a failed row for every file.
async fn reach(ctx: &Context, prefix: &S3Uri) -> Result<(), Error> {
    tokio::select! {
        page = ctx.s3.list_objects(prefix.bucket(), prefix.key(), true, None, 1) => {
            page.map(|_| ())
        }
        () = ctx.stop.stopped() => Err(Error::Cancelled),
    }
}

/// Reports the rows of a plan that is not executed: a dry run, or a plan
/// whose rows are all equal or failed.
fn report_only(
    plan: SyncPlan,
    dry_run: bool,
    report_since: Option<Instant>,
    out: &mut Output,
) -> Result<(), Failure> {
    let mut reporter = Reporter::new(out, plan, None, dry_run);
    reporter.report_since = report_since;
    reporter.report_rows(|row| row.action != SyncAction::Equal);
    reporter.finish(None, false)
}

/// What a finished row reports: the executor sends the row in its new state.
struct RowEvents(mpsc::UnboundedSender<(usize, SyncRow)>);

impl SyncEvents for RowEvents {
    fn progress(&self, _progress: &SyncProgress) {
        // The numbers of the run are counted from the rows.
    }

    fn row(&self, _plan_id: &str, index: usize, row: &SyncRow) {
        // A closed channel means the command is over.
        let _ = self.0.send((index, row.clone()));
    }
}

/// Runs a stored plan, reporting each row as it finishes. Ctrl-C (or a closed
/// standard output) cancels the run through the executor, which cleans up and
/// leaves the plan resumable.
async fn execute(
    ctx: &Context,
    store: Arc<SyncStore>,
    engine: TransferEngine,
    plan: SyncPlan,
    report_since: Option<Instant>,
    out: &mut Output,
) -> Result<(), Failure> {
    let plan_id = plan.id.clone();
    let mut reporter = Reporter::new(out, plan, Some(plan_id.clone()), false);
    reporter.report_since = report_since;
    if ctx.show_progress {
        reporter.show_progress();
    }
    // Rows the analysis could not plan never run; they are reported first.
    reporter.report_rows(|row| row.action == SyncAction::Failed);

    let executor = SyncExecutor::new(store);
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let run = executor.start(
        &plan_id,
        engine,
        ExecuteOptions::standalone(&ctx.settings),
        Arc::new(RowEvents(sender)),
    )?;
    let mut run = pin!(run.run());
    let mut interrupted = false;
    let result = loop {
        tokio::select! {
            result = &mut run => break result,
            Some((index, row)) = receiver.recv() => reporter.finished(index, row),
            () = ctx.stop.stopped(), if !interrupted => {
                interrupted = true;
                // The engines abort multipart uploads and remove partial downloads.
                let _ = executor.cancel(&plan_id);
                reporter.interrupted();
            }
        }
        if reporter.is_broken() && !interrupted {
            // Nobody reads the results any more.
            interrupted = true;
            let _ = executor.cancel(&plan_id);
        }
    };
    // The results recorded by the last batch of the run.
    while let Ok((index, row)) = receiver.try_recv() {
        reporter.finished(index, row);
    }
    let summary = match result {
        Ok(summary) => summary,
        Err(err) => {
            reporter.close();
            return Err(err
                .with_context(trf("sync plan {plan_id}", &[("plan_id", &plan_id)]))
                .into());
        }
    };
    // A plan that ran to the end is not needed any more; one with failed or
    // unfinished rows stays for `--resume`.
    let complete = !summary.cancelled && sync::row_counts(&reporter.plan.rows).pending() == 0;
    let kept = if complete {
        let store = Arc::clone(executor.store());
        // A leftover finished plan is harmless, so a failure is not reported.
        let _ = blocking(move || store.delete(&plan_id).map(|()| plan_id)).await;
        None
    } else {
        Some(plan_id)
    };
    reporter.finish(kept, summary.cancelled)
}

/// What a row does, from its action and target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    Upload,
    Download,
    DeleteRemote,
    DeleteLocal,
    /// A server-side copy between two S3 prefixes.
    Copy,
    /// A row the analysis could not plan.
    Unsyncable,
}

fn operation(row: &SyncRow) -> Operation {
    match (row.action, row.target) {
        (SyncAction::Copy, Some(Side::Remote)) => Operation::Upload,
        (SyncAction::Copy, Some(Side::Local)) => Operation::Download,
        (SyncAction::Delete, Some(Side::Remote)) => Operation::DeleteRemote,
        (SyncAction::Delete, Some(Side::Local)) => Operation::DeleteLocal,
        _ => Operation::Unsyncable,
    }
}

/// The size of the file a row copies or deletes, when the plan knows it.
fn row_size(row: &SyncRow) -> Option<u64> {
    match operation(row) {
        Operation::Upload | Operation::Copy | Operation::DeleteLocal => {
            row.local_meta.as_ref().map(|m| m.size)
        }
        Operation::Download | Operation::DeleteRemote => row.s3_meta.as_ref().map(|m| m.size),
        Operation::Unsyncable => None,
    }
}

/// Where the rows of one plan live: the local folder and the S3 prefix.
struct Layout {
    plan_id: Option<String>,
    root: PathBuf,
    bucket: String,
    prefix: String,
    /// Bucket and prefix of the source of an S3 to S3 sync.
    source: Option<(String, String)>,
}

impl Layout {
    fn new(plan: &SyncPlan, plan_id: Option<String>) -> Self {
        Layout {
            plan_id,
            root: plan.local_root.clone(),
            bucket: plan.s3_prefix.bucket().to_owned(),
            prefix: plan.s3_prefix.key().to_owned(),
            source: plan
                .source_prefix
                .as_ref()
                .map(|uri| (uri.bucket().to_owned(), uri.key().to_owned())),
        }
    }

    /// What a row does here: an upload in an S3 to S3 plan is a copy.
    fn operation(&self, row: &SyncRow) -> Operation {
        match operation(row) {
            Operation::Upload if self.source.is_some() => Operation::Copy,
            other => other,
        }
    }

    /// The `s3://` URI of the source object of an S3 to S3 row.
    fn source_uri(&self, rel_path: &str) -> Option<String> {
        self.source
            .as_ref()
            .map(|(bucket, prefix)| format!("s3://{bucket}/{}", join_key(prefix, rel_path)))
    }

    /// The local file of a row, with the separators of this OS.
    fn local(&self, rel_path: &str) -> String {
        let mut path = self.root.clone();
        path.extend(rel_path.split('/'));
        path.display().to_string()
    }

    /// The `s3://` URI of a row's object.
    fn remote(&self, rel_path: &str) -> String {
        format!("s3://{}/{}", self.bucket, join_key(&self.prefix, rel_path))
    }

    fn record(&self, row: &SyncRow, dry_run: bool) -> SyncRowRecord {
        let operation = self.operation(row);
        let (local, remote, source) = match operation {
            Operation::Unsyncable => (None, None, None),
            Operation::Copy => (
                None,
                Some(self.remote(&row.rel_path)),
                self.source_uri(&row.rel_path),
            ),
            _ => (
                Some(self.local(&row.rel_path)),
                Some(self.remote(&row.rel_path)),
                None,
            ),
        };
        SyncRowRecord {
            plan_id: self.plan_id.clone(),
            path: row.rel_path.clone(),
            action: row.action,
            target: row.target,
            local,
            remote,
            source,
            size: row_size(row),
            dry_run,
            done: row.done,
            error: row.error.clone(),
        }
    }

    /// `uploaded a/b.txt to s3://...`, or with `--dry-run` `would upload ...`.
    fn success_line(&self, row: &SyncRow, dry_run: bool) -> String {
        let (local, remote) = (self.local(&row.rel_path), self.remote(&row.rel_path));
        let source = self.source_uri(&row.rel_path).unwrap_or_default();
        let template = |would: &'static str, done: &'static str| {
            if dry_run { would } else { done }
        };
        match self.operation(row) {
            Operation::Copy => trf(
                template(
                    "would copy {source} to {destination}",
                    "copied {source} to {destination}",
                ),
                &[
                    ("source", source.as_str()),
                    ("destination", remote.as_str()),
                ],
            ),
            Operation::Upload => trf(
                template(
                    "would upload {source} to {destination}",
                    "uploaded {source} to {destination}",
                ),
                &[("source", local.as_str()), ("destination", remote.as_str())],
            ),
            Operation::Download => trf(
                template(
                    "would download {source} to {destination}",
                    "downloaded {source} to {destination}",
                ),
                &[("source", remote.as_str()), ("destination", local.as_str())],
            ),
            Operation::DeleteRemote => trf(
                template("would delete {source}", "deleted {source}"),
                &[("source", remote.as_str())],
            ),
            Operation::DeleteLocal => trf(
                template("would delete {source}", "deleted {source}"),
                &[("source", local.as_str())],
            ),
            Operation::Unsyncable => trf("cannot sync {path}", &[("path", &row.rel_path)]),
        }
    }

    /// `failed to upload a/b.txt to s3://...: why`, printed as `bb: ...`.
    fn failure_line(&self, row: &SyncRow) -> String {
        let (local, remote) = (self.local(&row.rel_path), self.remote(&row.rel_path));
        let why = row.error.as_deref().unwrap_or("unknown error");
        let source = self.source_uri(&row.rel_path).unwrap_or_default();
        match self.operation(row) {
            Operation::Copy => trf(
                "failed to copy {source} to {destination}: {why}",
                &[
                    ("source", source.as_str()),
                    ("destination", remote.as_str()),
                    ("why", why),
                ],
            ),
            Operation::Upload => trf(
                "failed to upload {source} to {destination}: {why}",
                &[
                    ("source", local.as_str()),
                    ("destination", remote.as_str()),
                    ("why", why),
                ],
            ),
            Operation::Download => trf(
                "failed to download {source} to {destination}: {why}",
                &[
                    ("source", remote.as_str()),
                    ("destination", local.as_str()),
                    ("why", why),
                ],
            ),
            Operation::DeleteRemote => trf(
                "failed to delete {source}: {why}",
                &[("source", remote.as_str()), ("why", why)],
            ),
            Operation::DeleteLocal => trf(
                "failed to delete {source}: {why}",
                &[("source", local.as_str()), ("why", why)],
            ),
            Operation::Unsyncable => trf(
                "cannot sync {path}: {why}",
                &[("path", &row.rel_path), ("why", why)],
            ),
        }
    }
}

/// Every row of a plan counted in exactly one place.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Tally {
    equal: usize,
    copied: usize,
    deleted: usize,
    /// Rows the analysis could not plan.
    unsyncable: usize,
    /// Rows that failed while running (`--resume` tries them again).
    retryable: usize,
    pending_copy: usize,
    pending_delete: usize,
}

impl Tally {
    fn of(rows: &[SyncRow]) -> Self {
        let mut tally = Tally::default();
        for row in rows {
            match (row.action, row.done, row.error.is_some()) {
                (SyncAction::Equal, ..) => tally.equal += 1,
                (SyncAction::Failed, ..) => tally.unsyncable += 1,
                (SyncAction::Copy, true, _) => tally.copied += 1,
                (SyncAction::Delete, true, _) => tally.deleted += 1,
                (_, false, true) => tally.retryable += 1,
                (SyncAction::Copy, false, false) => tally.pending_copy += 1,
                (SyncAction::Delete, false, false) => tally.pending_delete += 1,
            }
        }
        tally
    }

    fn failed(&self) -> usize {
        self.unsyncable + self.retryable
    }

    fn pending(&self) -> usize {
        self.pending_copy + self.pending_delete
    }

    /// Rows that needed something: everything but the equal rows.
    fn needing_work(&self) -> usize {
        self.copied + self.deleted + self.failed() + self.pending()
    }
}

/// The `--summary` report in the human format: one indented line per action.
fn report_text(report: &SyncReport) -> String {
    let mut lines = vec![tr("summary:")];
    lines.extend(report.lines().into_iter().map(|line| format!("  {line}")));
    lines.join("\n")
}

/// The one line that ends a run in the human format.
fn summary_line(tally: &Tally, dry_run: bool) -> String {
    let mut line = if dry_run {
        trf(
            "would copy {copy}, delete {delete}; {equal} already in sync",
            &[
                ("copy", tally.pending_copy.to_string().as_str()),
                ("delete", tally.pending_delete.to_string().as_str()),
                ("equal", tally.equal.to_string().as_str()),
            ],
        )
    } else {
        trf(
            "{copied} copied, {deleted} deleted, {equal} already in sync",
            &[
                ("copied", tally.copied.to_string().as_str()),
                ("deleted", tally.deleted.to_string().as_str()),
                ("equal", tally.equal.to_string().as_str()),
            ],
        )
    };
    if tally.failed() > 0 {
        line.push_str(&trf(
            ", {failed} failed",
            &[("failed", tally.failed().to_string().as_str())],
        ));
    }
    if !dry_run && tally.pending() > 0 {
        line.push_str(&trf(
            ", {pending} not done",
            &[("pending", tally.pending().to_string().as_str())],
        ));
    }
    line
}

/// Prints the rows and the summary, above the progress line when there is
/// one. The plan's rows follow the results, so the summary counts the final
/// state.
struct Reporter<'o> {
    out: &'o mut Output,
    line: Option<ProgressLine>,
    layout: Layout,
    plan: SyncPlan,
    dry_run: bool,
    totals: Option<Arc<Totals>>,
    /// Writing to standard output failed: nothing more is printed.
    broken: Option<Failure>,
    /// `--summary`: when the command started, for the report at the end.
    report_since: Option<Instant>,
}

impl<'o> Reporter<'o> {
    fn new(out: &'o mut Output, plan: SyncPlan, plan_id: Option<String>, dry_run: bool) -> Self {
        Reporter {
            out,
            line: None,
            layout: Layout::new(&plan, plan_id),
            plan,
            dry_run,
            totals: None,
            broken: None,
            report_since: None,
        }
    }

    /// Draws the progress of the rows still to run on standard error. The
    /// executor reports whole rows, so bytes count when a copy is finished.
    fn show_progress(&mut self) {
        let rows = &self.plan.rows;
        let bytes = rows
            .iter()
            .filter(|row| sync::row_is_pending(row))
            .filter(|row| {
                matches!(
                    self.layout.operation(row),
                    Operation::Upload | Operation::Download | Operation::Copy
                )
            })
            .filter_map(row_size)
            .sum();
        let totals = Totals::new(sync::row_counts(rows).pending(), bytes);
        self.line = Some(ProgressLine::start(
            Arc::clone(&totals),
            self.out.stderr().clone(),
        ));
        self.totals = Some(totals);
    }

    fn is_broken(&self) -> bool {
        self.broken.is_some()
    }

    fn print(&mut self, write: impl FnOnce(&mut Output) -> Result<(), Failure>) {
        if self.broken.is_some() {
            return;
        }
        let out = &mut *self.out;
        if let Err(failure) = print_above(self.line.as_ref(), || write(out)) {
            self.broken = Some(failure);
        }
    }

    /// One row: a record, or a failure.
    fn report(&mut self, row: &SyncRow) {
        let record = SyncRecord::Row(self.layout.record(row, self.dry_run));
        if sync::row_is_failed(row) {
            let text = self.layout.failure_line(row);
            self.print(|out| out.item_failed(&record, || text));
        } else {
            let text = self.layout.success_line(row, self.dry_run);
            self.print(|out| out.change(&record, || text));
        }
    }

    /// Reports the rows of the plan that `wanted` selects, in path order.
    fn report_rows(&mut self, wanted: impl Fn(&SyncRow) -> bool) {
        let rows = std::mem::take(&mut self.plan.rows);
        for row in rows.iter().filter(|row| wanted(row)) {
            self.report(row);
        }
        self.plan.rows = rows;
    }

    /// The executor finished (or failed) row `index`.
    fn finished(&mut self, index: usize, row: SyncRow) {
        if let Some(totals) = &self.totals {
            totals.item_finished();
            if matches!(
                self.layout.operation(&row),
                Operation::Upload | Operation::Download | Operation::Copy
            ) {
                let size = row_size(&row).unwrap_or(0);
                let progress = ItemProgress::new(Arc::clone(totals), size);
                if row.done {
                    progress.add_done(size);
                } else {
                    progress.set_total(0);
                }
            }
        }
        self.report(&row);
        if let Some(slot) = self.plan.rows.get_mut(index) {
            *slot = row;
        }
    }

    /// Ctrl-C: say how to continue, before the run has cleaned up.
    fn interrupted(&mut self) {
        let Some(plan_id) = &self.layout.plan_id else {
            return;
        };
        let text = trf(
            "bb: interrupted; stopping. Resume with: bb sync --resume {plan_id}",
            &[("plan_id", plan_id)],
        );
        let out = &*self.out;
        print_above(self.line.as_ref(), || out.notice(&text));
    }

    /// Clears the progress line.
    fn close(&mut self) {
        drop(self.line.take());
    }

    /// Prints the summary and decides how the command ends. `kept` is the
    /// plan left in the store.
    fn finish(mut self, kept: Option<String>, cancelled: bool) -> Result<(), Failure> {
        self.close();
        if let Some(failure) = self.broken.take() {
            return Err(failure);
        }
        let tally = Tally::of(&self.plan.rows);
        if let Some(plan_id) = &kept
            && !cancelled
            && tally.retryable > 0
        {
            self.out.notice(&trf(
                "bb: {failed} failed while running; try again with: bb sync --resume {plan_id}",
                &[
                    ("failed", tally.retryable.to_string().as_str()),
                    ("plan_id", plan_id),
                ],
            ));
        }
        let record = SyncRecord::Summary(SyncSummaryRecord {
            plan_id: kept,
            direction: self.plan.direction,
            delete: self.plan.delete,
            local: self.plan.source_prefix.as_ref().map_or_else(
                || self.plan.local_root.display().to_string(),
                ToString::to_string,
            ),
            remote: self.plan.s3_prefix.to_string(),
            dry_run: self.dry_run,
            cancelled,
            equal: tally.equal,
            copied: tally.copied,
            deleted: tally.deleted,
            failed: tally.failed(),
            pending: tally.pending(),
        });
        let text = summary_line(&tally, self.dry_run);
        // A run that did not go well is never silent, even with --quiet.
        if cancelled || tally.failed() > 0 {
            self.out.row(&record, || text)?;
        } else {
            self.out.change(&record, || text)?;
        }
        if let Some(started) = self.report_since {
            let report = SyncReport::from_rows(
                &self.plan.id,
                self.plan.direction,
                &self.plan.rows,
                started.elapsed(),
                cancelled,
            );
            let text = report_text(&report);
            self.out.row(&SyncRecord::Report(report), || text)?;
        }
        if cancelled {
            return Err(Error::Cancelled.into());
        }
        if tally.failed() > 0 {
            return Err(Error::Remote(trf(
                "{failed} of {total} files could not be synced",
                &[
                    ("failed", tally.failed().to_string().as_str()),
                    ("total", tally.needing_work().to_string().as_str()),
                ],
            ))
            .into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use bucketbridge_core::ErrorKind;
    use bucketbridge_core::model::{Checksums, FileMeta, ObjectEntry};
    use chrono::{TimeZone, Utc};
    use clap::Parser;
    use serde_json::json;

    use super::*;
    use crate::cli::{Cli, Command};
    use crate::i18n::trf;

    fn args(extra: &[&str]) -> SyncArgs {
        let cli = Cli::try_parse_from(["bb", "sync"].into_iter().chain(extra.iter().copied()))
            .unwrap_or_else(|err| panic!("{extra:?}: {err}"));
        match cli.command {
            Command::Sync(args) => args,
            other => panic!("sync expected, got {other:?}"),
        }
    }

    fn row(rel_path: &str, action: SyncAction, target: Option<Side>) -> SyncRow {
        SyncRow {
            rel_path: rel_path.to_owned(),
            action,
            target,
            local_meta: Some(FileMeta {
                size: 3,
                modified: Utc.with_ymd_and_hms(2026, 10, 29, 8, 15, 0).single(),
            }),
            s3_meta: Some(ObjectEntry {
                key: format!("backup/{rel_path}"),
                size: 5,
                etag: None,
                last_modified: None,
                storage_class: None,
                checksums: Checksums::default(),
                is_prefix: false,
            }),
            error: None,
            done: false,
        }
    }

    fn layout(plan_id: Option<&str>) -> Layout {
        Layout {
            plan_id: plan_id.map(str::to_owned),
            root: PathBuf::from("root"),
            bucket: "bucket".to_owned(),
            prefix: "backup/".to_owned(),
            source: None,
        }
    }

    #[test]
    fn the_target_follows_the_arguments() {
        for (extra, direction, delete) in [
            (&["dir", "s3://b/p"][..], SyncDirection::Upload, false),
            (
                &["dir", "s3://b/p", "--delete"][..],
                SyncDirection::Upload,
                true,
            ),
            (
                &["dir", "s3://b", "--direction", "download", "--delete"][..],
                SyncDirection::Download,
                true,
            ),
            (
                &["--direction", "bidirectional", "dir", "s3://b/p/"][..],
                SyncDirection::Bidirectional,
                false,
            ),
        ] {
            let parsed = args(extra);
            match target(&parsed).unwrap() {
                Target::Plan {
                    direction: found,
                    delete: found_delete,
                    ..
                } => assert_eq!((found, found_delete), (direction, delete), "{extra:?}"),
                Target::Resume(_) | Target::Job(_) => panic!("{extra:?} is not a plan"),
            }
        }
        let id = "5b0e6a7a-3f1c-4d2e-9a47-0c8d2f6e1b35";
        let parsed = args(&["--resume", id]);
        assert!(matches!(target(&parsed).unwrap(), Target::Resume(found) if found == id));
        assert!(check(&parsed).is_ok());
    }

    #[test]
    fn bidirectional_with_delete_is_a_usage_error() {
        let parsed = args(&[
            "dir",
            "s3://b/p",
            "--direction",
            "bidirectional",
            "--delete",
        ]);
        let err = check(&parsed).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage);
        assert!(err.to_string().contains("--delete"), "{err}");
    }

    #[test]
    fn operations_are_named_by_action_and_target() {
        let cases = [
            (SyncAction::Copy, Some(Side::Remote), Operation::Upload),
            (SyncAction::Copy, Some(Side::Local), Operation::Download),
            (
                SyncAction::Delete,
                Some(Side::Remote),
                Operation::DeleteRemote,
            ),
            (
                SyncAction::Delete,
                Some(Side::Local),
                Operation::DeleteLocal,
            ),
            (SyncAction::Failed, None, Operation::Unsyncable),
        ];
        for (action, target, expected) in cases {
            assert_eq!(operation(&row("x", action, target)), expected);
        }
        // The size is that of the file that is copied or deleted.
        assert_eq!(
            row_size(&row("x", SyncAction::Copy, Some(Side::Remote))),
            Some(3)
        );
        assert_eq!(
            row_size(&row("x", SyncAction::Copy, Some(Side::Local))),
            Some(5)
        );
        assert_eq!(
            row_size(&row("x", SyncAction::Delete, Some(Side::Remote))),
            Some(5)
        );
        assert_eq!(
            row_size(&row("x", SyncAction::Delete, Some(Side::Local))),
            Some(3)
        );
        assert_eq!(row_size(&row("x", SyncAction::Failed, None)), None);
    }

    #[test]
    fn lines_name_both_sides() {
        // The values are data; asserting through the same templates as
        // the code keeps the test language-agnostic.
        let layout = layout(Some("p"));
        // The separators of this OS below the folder.
        let local = |rel: &str| {
            let mut path = PathBuf::from("root");
            path.extend(rel.split('/'));
            path.display().to_string()
        };
        let upload = row("a/b.txt", SyncAction::Copy, Some(Side::Remote));
        let src = local("a/b.txt");
        let dst = "s3://bucket/backup/a/b.txt";
        assert_eq!(
            layout.success_line(&upload, false),
            trf(
                "uploaded {source} to {destination}",
                &[("source", src.as_str()), ("destination", dst)]
            )
        );
        assert_eq!(
            layout.success_line(&upload, true),
            trf(
                "would upload {source} to {destination}",
                &[("source", src.as_str()), ("destination", dst)]
            )
        );
        let download = row("a/b.txt", SyncAction::Copy, Some(Side::Local));
        assert_eq!(
            layout.success_line(&download, false),
            trf(
                "downloaded {source} to {destination}",
                &[("source", dst), ("destination", src.as_str())]
            )
        );
        let remote_delete = row("old.txt", SyncAction::Delete, Some(Side::Remote));
        assert_eq!(
            layout.success_line(&remote_delete, true),
            trf(
                "would delete {source}",
                &[("source", "s3://bucket/backup/old.txt")]
            )
        );
        let local_delete = row("gone.txt", SyncAction::Delete, Some(Side::Local));
        assert_eq!(
            layout.success_line(&local_delete, false),
            trf(
                "deleted {source}",
                &[("source", local("gone.txt").as_str())]
            )
        );

        let mut failed = upload;
        failed.error = Some("access denied".to_owned());
        assert_eq!(
            layout.failure_line(&failed),
            trf(
                "failed to upload {source} to {destination}: {why}",
                &[
                    ("source", src.as_str()),
                    ("destination", dst),
                    ("why", "access denied")
                ]
            )
        );
        let mut unplanned = row("c.txt", SyncAction::Failed, None);
        unplanned.error = Some("cannot be read".to_owned());
        assert_eq!(
            layout.failure_line(&unplanned),
            trf(
                "cannot sync {path}: {why}",
                &[("path", "c.txt"), ("why", "cannot be read")]
            )
        );
    }

    #[test]
    fn records_have_stable_field_names() {
        let layout = layout(Some("plan"));
        let mut done = row("a.txt", SyncAction::Copy, Some(Side::Remote));
        done.done = true;
        let record = SyncRecord::Row(layout.record(&done, false));
        assert_eq!(
            serde_json::to_value(&record).unwrap(),
            json!({
                "type": "row",
                "plan_id": "plan",
                "path": "a.txt",
                "action": "copy",
                "target": "remote",
                "local": Path::new("root").join("a.txt").display().to_string(),
                "remote": "s3://bucket/backup/a.txt",
                "size": 3,
                "dry_run": false,
                "done": true,
                "error": null
            })
        );
        let mut unplanned = row("b.txt", SyncAction::Failed, None);
        unplanned.error = Some("cannot be read".to_owned());
        let record = SyncRecord::Row(layout.record(&unplanned, true));
        assert_eq!(
            serde_json::to_value(&record).unwrap(),
            json!({
                "type": "row",
                "plan_id": "plan",
                "path": "b.txt",
                "action": "failed",
                "target": null,
                "local": null,
                "remote": null,
                "size": null,
                "dry_run": true,
                "done": false,
                "error": "cannot be read"
            })
        );
        let summary = SyncRecord::Summary(SyncSummaryRecord {
            plan_id: None,
            direction: SyncDirection::Bidirectional,
            delete: false,
            local: "/x".to_owned(),
            remote: "s3://b/p/".to_owned(),
            dry_run: false,
            cancelled: false,
            equal: 4,
            copied: 3,
            deleted: 2,
            failed: 1,
            pending: 0,
        });
        assert_eq!(
            serde_json::to_value(&summary).unwrap(),
            json!({
                "type": "summary",
                "plan_id": null,
                "direction": "bidirectional",
                "delete": false,
                "local": "/x",
                "remote": "s3://b/p/",
                "dry_run": false,
                "cancelled": false,
                "equal": 4,
                "copied": 3,
                "deleted": 2,
                "failed": 1,
                "pending": 0
            })
        );
    }

    #[test]
    fn the_tally_counts_every_row_once() {
        let make = |action: SyncAction, done: bool, error: Option<&str>| {
            let mut row = row("x", action, Some(Side::Remote));
            row.done = done;
            row.error = error.map(str::to_owned);
            row
        };
        let rows = vec![
            make(SyncAction::Equal, true, None),
            make(SyncAction::Equal, true, None),
            make(SyncAction::Copy, true, None),
            make(SyncAction::Copy, false, None),
            make(SyncAction::Copy, false, Some("timeout")),
            make(SyncAction::Delete, true, None),
            make(SyncAction::Delete, false, None),
            make(SyncAction::Delete, false, Some("denied")),
            make(SyncAction::Failed, false, Some("unreadable")),
        ];
        let tally = Tally::of(&rows);
        assert_eq!(
            tally,
            Tally {
                equal: 2,
                copied: 1,
                deleted: 1,
                unsyncable: 1,
                retryable: 2,
                pending_copy: 1,
                pending_delete: 1,
            }
        );
        assert_eq!(
            (tally.failed(), tally.pending(), tally.needing_work()),
            (3, 2, 7)
        );
        // The same rows as the core counts them.
        let counts = sync::row_counts(&rows);
        assert_eq!(counts.failed, tally.failed());
        assert_eq!(counts.pending(), tally.pending() + tally.retryable);
    }

    #[test]
    fn summary_lines() {
        // Asserted through the same templates and values as the code.
        let base = |copied: usize, deleted: usize, equal: usize| {
            trf(
                "{copied} copied, {deleted} deleted, {equal} already in sync",
                &[
                    ("copied", copied.to_string().as_str()),
                    ("deleted", deleted.to_string().as_str()),
                    ("equal", equal.to_string().as_str()),
                ],
            )
        };
        let tally = Tally {
            equal: 7,
            copied: 3,
            deleted: 1,
            ..Tally::default()
        };
        let mut expected = base(3, 1, 7);
        expected.push_str(&trf(", {failed} failed", &[("failed", "2")]));
        expected.push_str(&trf(", {pending} not done", &[("pending", "3")]));
        let troubled = Tally {
            unsyncable: 1,
            retryable: 1,
            pending_copy: 2,
            pending_delete: 1,
            ..tally
        };
        assert_eq!(summary_line(&troubled, false), expected);
        let mut dry_expected = trf(
            "would copy {copy}, delete {delete}; {equal} already in sync",
            &[("copy", "2"), ("delete", "1"), ("equal", "7")],
        );
        dry_expected.push_str(&trf(", {failed} failed", &[("failed", "2")]));
        assert_eq!(summary_line(&troubled, true), dry_expected);
        assert_eq!(summary_line(&tally, false), base(3, 1, 7));
    }
}
