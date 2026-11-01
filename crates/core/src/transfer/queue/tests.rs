//! Queue tests with fake runners. Every test runs on Tokio's paused clock, so
//! the timings asserted below are exact virtual times and do not depend on
//! machine speed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::VecDeque;
use std::fs;
use std::sync::atomic::AtomicUsize;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::error::TryRecvError;

use super::*;

const SEQ: Ordering = Ordering::SeqCst;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Checkpoint {
    next: u64,
}

/// How one run of a task misbehaves; consumed one per run.
#[derive(Debug, Clone, Copy)]
enum Plan {
    /// Fail with this kind when reaching the chunk.
    FailAt(u64, ErrorKind),
    /// Never finish and ignore stop requests.
    Hang,
    /// Panic at the start of the run.
    Panic,
}

/// Processes `chunks` chunks of `chunk_bytes`, one every `chunk_delay`,
/// checkpointing `{"next": n}` after each, and stops promptly on request.
struct ChunkRunner {
    chunks: u64,
    chunk_bytes: u64,
    chunk_delay: Duration,
    checkpoints: bool,
    plans: Mutex<HashMap<String, VecDeque<Plan>>>,
    /// Task name and number of active runs (including this one) per start.
    starts: Mutex<Vec<(String, usize)>>,
    processed: Mutex<Vec<(String, u64)>>,
    cleanups: Mutex<Vec<(String, Option<Value>)>>,
    fail_cleanup: AtomicBool,
    hang_cleanup: AtomicBool,
    active: AtomicUsize,
    peak: AtomicUsize,
    /// Runs dropped by the queue before they returned.
    dropped: AtomicUsize,
}

impl ChunkRunner {
    fn new(chunks: u64, chunk_bytes: u64, chunk_delay: Duration) -> Self {
        Self {
            chunks,
            chunk_bytes,
            chunk_delay,
            checkpoints: true,
            plans: Mutex::default(),
            starts: Mutex::default(),
            processed: Mutex::default(),
            cleanups: Mutex::default(),
            fail_cleanup: AtomicBool::new(false),
            hang_cleanup: AtomicBool::new(false),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
        }
    }

    fn shared(self) -> Arc<Self> {
        Arc::new(self)
    }

    fn without_checkpoints(mut self) -> Self {
        self.checkpoints = false;
        self
    }

    fn plan(&self, name: &str, plans: &[Plan]) {
        self.plans
            .lock()
            .unwrap()
            .entry(name.to_owned())
            .or_default()
            .extend(plans.iter().copied());
    }

    fn starts(&self) -> Vec<String> {
        self.starts
            .lock()
            .unwrap()
            .iter()
            .map(|(name, _)| name.clone())
            .collect()
    }

    fn processed(&self, name: &str) -> Vec<u64> {
        self.processed
            .lock()
            .unwrap()
            .iter()
            .filter(|(task, _)| task == name)
            .map(|(_, chunk)| *chunk)
            .collect()
    }

    fn cleanups_of(&self, name: &str) -> Vec<Option<Value>> {
        self.cleanups
            .lock()
            .unwrap()
            .iter()
            .filter(|(task, _)| task == name)
            .map(|(_, resume)| resume.clone())
            .collect()
    }

    async fn process(&self, ctx: &TaskContext, name: &str, plan: Option<Plan>) -> Result<()> {
        if matches!(plan, Some(Plan::Hang)) {
            std::future::pending::<()>().await;
        }
        let start = ctx
            .resume()
            .get_as::<Checkpoint>()
            .map_err(|e| new_error(ErrorKind::Internal, e.to_string()))?
            .map_or(0, |checkpoint| checkpoint.next);
        ctx.progress()
            .set_total(Some(self.chunks * self.chunk_bytes));
        ctx.progress().set_done(start * self.chunk_bytes);
        for chunk in start..self.chunks {
            if let Some(Plan::FailAt(at, kind)) = plan
                && at == chunk
            {
                let message = format!("injected failure at chunk {chunk}");
                return Err(match kind {
                    ErrorKind::Io => io_error(message, std::io::Error::other("disk hiccup")),
                    kind => new_error(kind, message),
                });
            }
            tokio::select! {
                _ = ctx.stop().wait() => return ctx.stop().check(),
                () = tokio::time::sleep(self.chunk_delay) => {}
            }
            self.processed
                .lock()
                .unwrap()
                .push((name.to_owned(), chunk));
            ctx.progress().add_done(self.chunk_bytes);
            if self.checkpoints {
                ctx.resume()
                    .set_as(&Checkpoint { next: chunk + 1 })
                    .map_err(|e| new_error(ErrorKind::Internal, e.to_string()))?;
            }
        }
        Ok(())
    }
}

/// Counts a run as active while it exists; notices runs dropped mid-way.
struct Active<'a> {
    runner: &'a ChunkRunner,
    finished: bool,
}

impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.runner.active.fetch_sub(1, SEQ);
        if !self.finished {
            self.runner.dropped.fetch_add(1, SEQ);
        }
    }
}

impl TaskRunner for ChunkRunner {
    fn run(&self, ctx: TaskContext) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let name = name_of(ctx.task());
            let plan = self
                .plans
                .lock()
                .unwrap()
                .get_mut(&name)
                .and_then(VecDeque::pop_front);
            if matches!(plan, Some(Plan::Panic)) {
                panic!("injected panic");
            }
            let active = self.active.fetch_add(1, SEQ) + 1;
            self.peak.fetch_max(active, SEQ);
            self.starts.lock().unwrap().push((name.clone(), active));
            let mut guard = Active {
                runner: self,
                finished: false,
            };
            let result = self.process(&ctx, &name, plan).await;
            guard.finished = true;
            result
        })
    }

    fn cleanup(&self, task: TransferTask, resume: Option<Value>) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.cleanups.lock().unwrap().push((name_of(&task), resume));
            if self.hang_cleanup.load(SEQ) {
                std::future::pending::<()>().await;
            }
            if self.fail_cleanup.load(SEQ) {
                Err(new_error(ErrorKind::Remote, "abort failed"))
            } else {
                Ok(())
            }
        })
    }
}

/// Pushes `bytes` through the shared limiter in pieces, reporting progress.
struct ThrottleRunner {
    bytes: u64,
    piece: u64,
}

impl TaskRunner for ThrottleRunner {
    fn run(&self, ctx: TaskContext) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            ctx.progress().set_total(Some(self.bytes));
            let mut sent = ctx.progress().done();
            while sent < self.bytes {
                let piece = self.piece.min(self.bytes - sent);
                tokio::select! {
                    _ = ctx.stop().wait() => return ctx.stop().check(),
                    () = ctx.limiter().acquire(piece) => {}
                }
                sent += piece;
                ctx.progress().add_done(piece);
            }
            Ok(())
        })
    }
}

fn name_of(task: &TransferTask) -> String {
    match &task.src {
        Location::Local { path } => path.display().to_string(),
        Location::S3 { key, .. } => key.clone(),
    }
}

fn upload(account: &str, name: &str) -> NewTask {
    NewTask {
        account_id: account.to_owned(),
        group_id: None,
        kind: TransferKind::Upload,
        src: Location::Local {
            path: PathBuf::from(name),
        },
        dst: Some(Location::S3 {
            bucket: "bucket".to_owned(),
            key: name.to_owned(),
        }),
        bytes_total: None,
    }
}

fn settings(concurrency: u32, retries: u32) -> TransferSettings {
    TransferSettings {
        max_concurrent_tasks: concurrency,
        retry_attempts: retries,
        ..TransferSettings::default()
    }
}

fn options() -> QueueOptions {
    QueueOptions {
        jitter_seed: Some(7),
        ..QueueOptions::default()
    }
}

async fn open_with(
    runner: Arc<dyn TaskRunner>,
    settings: TransferSettings,
    options: QueueOptions,
) -> TransferQueue {
    TransferQueue::open(settings, Runners::for_all(runner), options)
        .await
        .unwrap()
}

async fn open(
    runner: &Arc<ChunkRunner>,
    settings: TransferSettings,
    options: QueueOptions,
) -> TransferQueue {
    open_with(Arc::clone(runner) as Arc<dyn TaskRunner>, settings, options).await
}

/// Lets virtual time pass in 1 ms steps until `check` holds.
async fn wait_until(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(600);
    while !check() {
        assert!(
            Instant::now() < deadline,
            "condition not reached within ten virtual minutes"
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

async fn wait_state(queue: &TransferQueue, id: &str, state: TransferState) -> TransferTask {
    wait_until(|| queue.get(id).unwrap().state == state).await;
    queue.get(id).unwrap()
}

/// The next event and the virtual time it was sent at (on the current-thread
/// test runtime the receiver runs before the clock moves on).
async fn next_event(events: &mut UnboundedReceiver<QueueEvent>) -> (Instant, QueueEvent) {
    let event = tokio::time::timeout(Duration::from_secs(600), events.recv())
        .await
        .expect("no event within ten virtual minutes")
        .expect("event stream closed");
    (Instant::now(), event)
}

fn drain(events: &mut UnboundedReceiver<QueueEvent>) -> Vec<QueueEvent> {
    let mut drained = Vec::new();
    while let Ok(event) = events.try_recv() {
        drained.push(event);
    }
    drained
}

fn assert_close(actual: Duration, expected: Duration, tolerance: Duration) {
    assert!(
        actual.abs_diff(expected) <= tolerance,
        "expected {expected:?} +/- {tolerance:?}, got {actual:?}"
    );
}

fn assert_conflict(result: Result<()>, message: &str) {
    let error = result.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Conflict, "{error}");
    assert!(error.to_string().contains(message), "{error}");
}

fn record(id: &str, kind: TransferKind, state: TransferState) -> StoredTask {
    let now = Utc::now();
    StoredTask {
        task: TransferTask {
            id: id.to_owned(),
            account_id: "acct".to_owned(),
            group_id: None,
            kind,
            src: Location::Local {
                path: PathBuf::from(id),
            },
            dst: (kind != TransferKind::Delete).then(|| Location::S3 {
                bucket: "bucket".to_owned(),
                key: id.to_owned(),
            }),
            bytes_total: Some(300),
            bytes_done: 100,
            speed: 1234,
            eta: Some(9),
            state,
            error: (state == TransferState::Failed).then(|| "earlier failure".to_owned()),
            attempts: 1,
            created_at: now,
            updated_at: now,
        },
        resume: Some(json!({"next": 1})),
        cancel_pending: false,
    }
}

#[tokio::test(start_paused = true)]
async fn added_tasks_are_listed_in_order_and_announced() {
    let runner = ChunkRunner::new(3, 10, Duration::from_millis(100)).shared();
    let queue = open(&runner, settings(1, 0), options()).await;
    let mut events = queue.subscribe();
    let added = queue
        .add_tasks(vec![
            upload("acct", "a"),
            upload("acct", "b"),
            upload("acct", "c"),
        ])
        .unwrap();
    assert_eq!(added.len(), 3);
    for task in &added {
        assert_eq!(task.state, TransferState::Queued);
        assert_eq!((task.attempts, task.bytes_done, task.speed), (0, 0, 0));
        assert!(
            uuid::Uuid::parse_str(&task.id).is_ok(),
            "id {} is a UUID",
            task.id
        );
    }
    let listed: Vec<(String, TransferState)> =
        queue.list().into_iter().map(|t| (t.id, t.state)).collect();
    assert_eq!(
        listed,
        vec![
            (added[0].id.clone(), TransferState::Running),
            (added[1].id.clone(), TransferState::Queued),
            (added[2].id.clone(), TransferState::Queued),
        ]
    );
    // Sent synchronously: three additions, then the start of the first task.
    let sent: Vec<(String, TransferState)> = drain(&mut events)
        .into_iter()
        .map(|event| match event {
            QueueEvent::Updated(task) => (task.id, task.state),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(sent.len(), 4);
    assert_eq!(sent[3], (added[0].id.clone(), TransferState::Running));
    assert_eq!(queue.get(&added[0].id).unwrap().attempts, 1);
    let single = queue.add_task(upload("acct", "d")).unwrap();
    assert_eq!(single.state, TransferState::Queued);
    let missing = queue.get("no-such-task").unwrap_err();
    assert_eq!(missing.kind(), ErrorKind::NotFound);
}

#[tokio::test(start_paused = true)]
async fn invalid_new_tasks_are_rejected_without_adding_any() {
    let runner = ChunkRunner::new(1, 1, Duration::from_millis(10)).shared();
    let runners = Runners::new().with(
        TransferKind::Upload,
        Arc::clone(&runner) as Arc<dyn TaskRunner>,
    );
    let queue = TransferQueue::open(settings(1, 0), runners, options())
        .await
        .unwrap();
    let blank_account = NewTask {
        account_id: " ".to_owned(),
        ..upload("acct", "a")
    };
    let delete_with_destination = NewTask {
        kind: TransferKind::Delete,
        ..upload("acct", "a")
    };
    let upload_without_destination = NewTask {
        dst: None,
        ..upload("acct", "a")
    };
    let download_without_runner = NewTask {
        kind: TransferKind::Download,
        ..upload("acct", "a")
    };
    for (bad, message) in [
        (blank_account, "account id"),
        (delete_with_destination, "delete tasks have no destination"),
        (
            upload_without_destination,
            "upload tasks need a destination",
        ),
        (
            download_without_runner,
            "no runner is registered for download tasks",
        ),
    ] {
        let error = queue
            .add_tasks(vec![upload("acct", "ok"), bad])
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Usage);
        assert!(error.to_string().contains(message), "{error}");
    }
    assert!(queue.list().is_empty(), "a rejected batch adds nothing");
}

#[tokio::test(start_paused = true)]
async fn tasks_can_be_added_as_failed_then_retried_or_cancelled() {
    let runner = ChunkRunner::new(3, 10, Duration::from_millis(100)).shared();
    let queue = open(&runner, settings(2, 0), options()).await;
    let mut events = queue.subscribe();
    let failed = |name: &str, next: u64| FailedTask {
        task: upload("acct", name),
        error: format!("{name} could not be read"),
        resume: Some(json!({ "next": next })),
    };
    let added = queue
        .add_tasks_with_failures(
            vec![upload("acct", "ok")],
            vec![failed("retried", 2), failed("cancelled", 1)],
        )
        .unwrap();
    let states: Vec<(TransferState, Option<&str>, u32)> = added
        .iter()
        .map(|t| (t.state, t.error.as_deref(), t.attempts))
        .collect();
    assert_eq!(
        states,
        vec![
            (TransferState::Queued, None, 0),
            (TransferState::Failed, Some("retried could not be read"), 0),
            (
                TransferState::Failed,
                Some("cancelled could not be read"),
                0
            ),
        ]
    );
    // Every task is announced as added; only the queued one starts.
    let announced: Vec<TransferState> = drain(&mut events)
        .into_iter()
        .map(|event| match event {
            QueueEvent::Updated(task) => task.state,
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(
        announced,
        vec![
            TransferState::Queued,
            TransferState::Failed,
            TransferState::Failed,
            TransferState::Running
        ]
    );
    wait_state(&queue, &added[0].id, TransferState::Completed).await;
    assert_eq!(runner.starts(), vec!["ok".to_owned()]);
    assert_eq!(
        queue.resume_state(&added[1].id).unwrap(),
        Some(json!({ "next": 2 }))
    );

    // A retry runs the runner with the given resume state.
    queue.retry_task(&added[1].id).unwrap();
    let retried = wait_state(&queue, &added[1].id, TransferState::Completed).await;
    assert_eq!((retried.error, retried.attempts), (None, 1));
    assert_eq!(runner.processed("retried"), vec![2]);

    // A cancel runs the cleanup with it.
    queue.cancel_task(&added[2].id).unwrap();
    wait_state(&queue, &added[2].id, TransferState::Cancelled).await;
    assert_eq!(
        runner.cleanups_of("cancelled"),
        vec![Some(json!({ "next": 1 }))]
    );
    assert!(!runner.starts().contains(&"cancelled".to_owned()));

    // Validation covers the failed tasks too, and a rejected batch adds
    // nothing.
    let before = queue.list().len();
    let invalid = FailedTask {
        task: NewTask {
            dst: None,
            ..upload("acct", "bad")
        },
        error: "x".into(),
        resume: None,
    };
    let error = queue
        .add_tasks_with_failures(vec![upload("acct", "fine")], vec![invalid])
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Usage);
    assert_eq!(queue.list().len(), before);
}

#[tokio::test(start_paused = true)]
async fn a_task_can_be_added_with_the_resume_state_its_first_run_gets() {
    let runner = ChunkRunner::new(4, 10, Duration::from_millis(100)).shared();
    let queue = open(&runner, settings(1, 0), options()).await;
    let task = queue
        .add_task_with_resume(upload("acct", "seeded"), json!({ "next": 3 }))
        .unwrap();
    assert_eq!(
        (task.state, task.attempts, task.error.as_deref()),
        (TransferState::Queued, 0, None)
    );
    wait_state(&queue, &task.id, TransferState::Completed).await;
    // The first run continued from the given state: only the last chunk.
    assert_eq!(runner.processed("seeded"), vec![3]);

    // The validation is that of add_tasks; nothing is added on an error.
    let before = queue.list().len();
    let invalid = NewTask {
        dst: None,
        ..upload("acct", "bad")
    };
    let error = queue
        .add_task_with_resume(invalid, json!({ "next": 1 }))
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Usage);
    assert_eq!(queue.list().len(), before);
}

#[tokio::test(start_paused = true)]
async fn concurrency_cap_is_never_exceeded_and_follows_live_changes() {
    let runner = ChunkRunner::new(3, 10, Duration::from_millis(100)).shared();
    let queue = open(&runner, settings(3, 0), options()).await;
    let first: Vec<NewTask> = (0..12)
        .map(|i| upload("acct", &format!("first-{i}")))
        .collect();
    let first = queue.add_tasks(first).unwrap();
    wait_until(|| {
        first
            .iter()
            .all(|t| queue.get(&t.id).unwrap().state == TransferState::Completed)
    })
    .await;
    assert_eq!(runner.peak.load(SEQ), 3);

    // Raising the cap starts more tasks at once.
    queue
        .update_settings(TransferSettings {
            max_concurrent_tasks: 5,
            ..settings(3, 0)
        })
        .unwrap();
    let second: Vec<NewTask> = (0..10)
        .map(|i| upload("acct", &format!("second-{i}")))
        .collect();
    let second = queue.add_tasks(second).unwrap();
    wait_until(|| runner.active.load(SEQ) == 5).await;
    assert_eq!(runner.peak.load(SEQ), 5);

    // Lowering it interrupts nothing; new runs start only below the new cap.
    queue.update_settings(settings(1, 0)).unwrap();
    let starts_before = runner.starts.lock().unwrap().len();
    wait_until(|| {
        second
            .iter()
            .all(|t| queue.get(&t.id).unwrap().state == TransferState::Completed)
    })
    .await;
    let later: Vec<usize> = runner.starts.lock().unwrap()[starts_before..]
        .iter()
        .map(|(_, active)| *active)
        .collect();
    assert_eq!(
        later,
        vec![1; 5],
        "after lowering the cap runs start one at a time"
    );
    assert!(
        queue.list().iter().all(|t| t.attempts == 1),
        "no run was interrupted"
    );
    assert_eq!(runner.dropped.load(SEQ), 0);
}

#[tokio::test(start_paused = true)]
async fn accounts_take_turns_and_each_account_is_first_in_first_out() {
    let runner = ChunkRunner::new(1, 1, Duration::from_millis(10)).shared();
    let queue = open(&runner, settings(1, 0), options()).await;
    queue
        .add_tasks(vec![
            upload("acct-a", "a1"),
            upload("acct-a", "a2"),
            upload("acct-a", "a3"),
            upload("acct-b", "b1"),
            upload("acct-b", "b2"),
        ])
        .unwrap();
    wait_until(|| {
        queue
            .list()
            .iter()
            .all(|t| t.state == TransferState::Completed)
    })
    .await;
    assert_eq!(runner.starts(), ["a1", "b1", "a2", "b2", "a3"]);

    // An account that shows up later gets the next free slot, whether its id
    // sorts after the last served account or before it.
    let runner = ChunkRunner::new(1, 1, Duration::from_millis(10)).shared();
    let queue = open(&runner, settings(1, 0), options()).await;
    queue
        .add_tasks(vec![
            upload("acct-m", "m1"),
            upload("acct-m", "m2"),
            upload("acct-m", "m3"),
        ])
        .unwrap();
    queue.add_task(upload("acct-z", "z1")).unwrap();
    queue.add_task(upload("acct-a", "a1")).unwrap();
    wait_until(|| {
        queue
            .list()
            .iter()
            .all(|t| t.state == TransferState::Completed)
    })
    .await;
    assert_eq!(runner.starts(), ["m1", "z1", "a1", "m2", "m3"]);
}

#[tokio::test(start_paused = true)]
async fn pause_and_resume_continue_from_the_resume_state() {
    let runner = ChunkRunner::new(10, 100, Duration::from_millis(100)).shared();
    let queue = open(&runner, settings(1, 0), options()).await;
    let a = queue.add_task(upload("acct", "a")).unwrap();
    let b = queue.add_task(upload("acct", "b")).unwrap();
    wait_until(|| runner.processed("a").len() >= 3).await;
    queue.pause_task(&a.id).unwrap();
    let paused = wait_state(&queue, &a.id, TransferState::Stopped).await;
    let next = runner.processed("a").len() as u64;
    assert_eq!(next, 3);
    assert_eq!(
        queue.resume_state(&a.id).unwrap(),
        Some(json!({"next": next}))
    );
    assert_eq!(paused.bytes_done, next * 100);
    assert_eq!(
        (paused.speed, paused.eta, paused.error.clone()),
        (0, None, None)
    );
    // The stopped task freed its slot for the next one.
    wait_state(&queue, &b.id, TransferState::Running).await;
    queue.resume_task(&a.id).unwrap();
    assert_eq!(queue.get(&a.id).unwrap().state, TransferState::Queued);
    let done = wait_state(&queue, &a.id, TransferState::Completed).await;
    assert_eq!(
        runner.processed("a"),
        (0..10).collect::<Vec<_>>(),
        "no chunk twice"
    );
    assert_eq!(done.attempts, 2);
    assert_eq!((done.bytes_done, done.bytes_total), (1000, Some(1000)));
    assert_eq!(
        queue.resume_state(&a.id).unwrap(),
        None,
        "cleared on completion"
    );
    assert_eq!(runner.starts(), ["a", "b", "a"]);
}

#[tokio::test(start_paused = true)]
async fn queued_tasks_pause_at_once_and_repeated_requests_conflict() {
    let runner = ChunkRunner::new(5, 10, Duration::from_millis(100)).shared();
    let queue = open(&runner, settings(1, 0), options()).await;
    let _running = queue.add_task(upload("acct", "running")).unwrap();
    let waiting = queue.add_task(upload("acct", "waiting")).unwrap();
    let mut events = queue.subscribe();
    queue.pause_task(&waiting.id).unwrap();
    // State changes are sent before the command returns.
    match events.try_recv() {
        Ok(QueueEvent::Updated(task)) => {
            assert_eq!(
                (task.id, task.state),
                (waiting.id.clone(), TransferState::Stopped)
            );
        }
        other => panic!("expected the stop event, got {other:?}"),
    }
    // Only the states listed by the contract are accepted.
    assert_conflict(
        queue.pause_task(&waiting.id),
        "cannot pause a task that is stopped",
    );
    assert_conflict(
        queue.retry_task(&waiting.id),
        "cannot retry a task that is stopped",
    );
    assert!(matches!(events.try_recv(), Err(TryRecvError::Empty)));
    queue.resume_task(&waiting.id).unwrap();
    assert_conflict(
        queue.resume_task(&waiting.id),
        "cannot resume a task that is queued",
    );
    assert_conflict(
        queue.retry_task(&waiting.id),
        "cannot retry a task that is queued",
    );
    assert_eq!(
        drain(&mut events).len(),
        1,
        "only the resume changed anything"
    );
    assert_eq!(queue.get(&waiting.id).unwrap().state, TransferState::Queued);
    wait_state(&queue, &waiting.id, TransferState::Completed).await;
}

#[tokio::test(start_paused = true)]
async fn cancelling_a_running_task_cleans_up_exactly_once() {
    let runner = ChunkRunner::new(10, 100, Duration::from_millis(100)).shared();
    let queue = open(&runner, settings(2, 0), options()).await;
    let a = queue.add_task(upload("acct", "a")).unwrap();
    wait_until(|| runner.processed("a").len() >= 2).await;
    queue.cancel_task(&a.id).unwrap();
    let cancelled = wait_state(&queue, &a.id, TransferState::Cancelled).await;
    assert_eq!(cancelled.error, None);
    assert_eq!(cancelled.bytes_done, 200);
    assert_eq!(runner.cleanups_of("a"), vec![Some(json!({"next": 2}))]);
    assert_eq!(queue.resume_state(&a.id).unwrap(), None);
    assert_conflict(
        queue.cancel_task(&a.id),
        "cannot cancel a task that is cancelled",
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        runner.cleanups_of("a").len(),
        1,
        "cleanup runs exactly once"
    );
    assert_eq!(runner.processed("a").len(), 2, "the run stopped promptly");
    assert_eq!(
        runner.dropped.load(SEQ),
        0,
        "the runner returned on its own"
    );
}

#[tokio::test(start_paused = true)]
async fn cancelling_idle_tasks_cleans_up_exactly_once() {
    let runner = ChunkRunner::new(10, 100, Duration::from_millis(100)).shared();
    runner.plan("failed", &[Plan::FailAt(1, ErrorKind::NotFound)]);
    let queue = open(&runner, settings(1, 0), options()).await;
    let failed = queue.add_task(upload("acct", "failed")).unwrap();
    wait_state(&queue, &failed.id, TransferState::Failed).await;
    let stopped = queue.add_task(upload("acct", "stopped")).unwrap();
    wait_until(|| runner.processed("stopped").len() >= 2).await;
    queue.pause_task(&stopped.id).unwrap();
    wait_state(&queue, &stopped.id, TransferState::Stopped).await;
    let blocker = queue.add_task(upload("acct", "blocker")).unwrap();
    let never = queue.add_task(upload("acct", "never")).unwrap();
    assert_eq!(queue.get(&never.id).unwrap().state, TransferState::Queued);
    for task in [&failed, &stopped, &never] {
        queue.cancel_task(&task.id).unwrap();
    }
    for task in [&failed, &stopped, &never] {
        let done = wait_state(&queue, &task.id, TransferState::Cancelled).await;
        assert_eq!(done.error, None);
        assert_eq!(queue.resume_state(&task.id).unwrap(), None);
    }
    assert_eq!(runner.cleanups_of("failed"), vec![Some(json!({"next": 1}))]);
    assert_eq!(
        runner.cleanups_of("stopped"),
        vec![Some(json!({"next": 2}))]
    );
    assert_eq!(runner.cleanups_of("never"), vec![None]);
    assert!(
        !runner.starts().contains(&"never".to_owned()),
        "a cancelled queued task never runs"
    );
    assert_eq!(
        queue.get(&blocker.id).unwrap().state,
        TransferState::Running
    );
    wait_state(&queue, &blocker.id, TransferState::Completed).await;
    assert!(runner.cleanups_of("blocker").is_empty());
}

/// The message of a task whose cancellation could not clean up.
fn kept_message(cause: &str) -> String {
    format!(
        "cleanup after cancelling failed: {cause}. The task was kept so that cancelling it again repeats the cleanup; it is also repeated when the app starts"
    )
}

#[tokio::test(start_paused = true)]
async fn a_failed_cleanup_keeps_the_task_failed_with_its_resume_state() {
    let runner = ChunkRunner::new(10, 100, Duration::from_millis(100)).shared();
    runner.fail_cleanup.store(true, SEQ);
    let queue = open(&runner, settings(1, 0), options()).await;
    let a = queue.add_task(upload("acct", "a")).unwrap();
    let b = queue.add_task(upload("acct", "b")).unwrap();
    wait_until(|| runner.processed("a").len() >= 2).await;
    queue.cancel_task(&a.id).unwrap();
    // The run was cancelled, the cleanup failed: the task is not cancelled
    // (terminal), it is failed, and the state that names what is left behind
    // is still there.
    let failed = wait_state(&queue, &a.id, TransferState::Failed).await;
    assert_eq!(
        failed.error.as_deref(),
        Some(kept_message("abort failed").as_str())
    );
    let kept = queue.resume_state(&a.id).unwrap();
    assert!(kept.is_some(), "the resume state was cleared");
    assert_eq!(runner.cleanups_of("a"), vec![kept.clone()]);
    // The slot is free again for the next task.
    wait_state(&queue, &b.id, TransferState::Running).await;

    // Cancelling the failed task again is accepted, repeats the cleanup with
    // the kept state, and keeps failing while the cleanup fails.
    queue.cancel_task(&a.id).unwrap();
    wait_until(|| runner.cleanups_of("a").len() == 2).await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    let again = queue.get(&a.id).unwrap();
    assert_eq!(again.state, TransferState::Failed);
    assert_eq!(again.error, failed.error);
    assert_eq!(queue.resume_state(&a.id).unwrap(), kept);
    assert_eq!(runner.cleanups_of("a"), vec![kept.clone(), kept.clone()]);

    // Once it can clean up, the task is cancelled for good.
    runner.fail_cleanup.store(false, SEQ);
    queue.cancel_task(&a.id).unwrap();
    let cancelled = wait_state(&queue, &a.id, TransferState::Cancelled).await;
    assert_eq!(cancelled.error, None);
    assert_eq!(queue.resume_state(&a.id).unwrap(), None);
    assert_eq!(runner.cleanups_of("a").len(), 3);
    assert_conflict(
        queue.cancel_task(&a.id),
        "cannot cancel a task that is cancelled",
    );
}

#[tokio::test(start_paused = true)]
async fn idle_tasks_whose_cleanup_fails_stay_failed_and_can_be_retried() {
    let runner = ChunkRunner::new(10, 100, Duration::from_millis(100)).shared();
    runner.plan("failed", &[Plan::FailAt(1, ErrorKind::NotFound)]);
    runner.fail_cleanup.store(true, SEQ);
    let queue = open(&runner, settings(1, 0), options()).await;
    let failed = queue.add_task(upload("acct", "failed")).unwrap();
    wait_state(&queue, &failed.id, TransferState::Failed).await;
    let stopped = queue.add_task(upload("acct", "stopped")).unwrap();
    wait_until(|| runner.processed("stopped").len() >= 2).await;
    queue.pause_task(&stopped.id).unwrap();
    wait_state(&queue, &stopped.id, TransferState::Stopped).await;
    let blocker = queue.add_task(upload("acct", "blocker")).unwrap();
    let never = queue.add_task(upload("acct", "never")).unwrap();
    assert_eq!(queue.get(&never.id).unwrap().state, TransferState::Queued);

    for task in [&failed, &stopped, &never] {
        queue.cancel_task(&task.id).unwrap();
    }
    for name in ["failed", "stopped", "never"] {
        wait_until(|| !runner.cleanups_of(name).is_empty()).await;
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
    for task in [&failed, &stopped, &never] {
        let kept = queue.get(&task.id).unwrap();
        assert_eq!(kept.state, TransferState::Failed, "{}", name_of(&kept));
        assert_eq!(
            kept.error.as_deref(),
            Some(kept_message("abort failed").as_str())
        );
    }
    assert_eq!(
        queue.resume_state(&stopped.id).unwrap(),
        Some(json!({"next": 2}))
    );
    assert!(!runner.starts().contains(&"never".to_owned()));

    // A failed task is retried as usual; starting it again drops the
    // cancellation, and it continues from the state that was kept.
    queue.retry_task(&stopped.id).unwrap();
    wait_state(&queue, &stopped.id, TransferState::Completed).await;
    assert_eq!(runner.processed("stopped"), (0..10).collect::<Vec<_>>());
    wait_state(&queue, &blocker.id, TransferState::Completed).await;

    runner.fail_cleanup.store(false, SEQ);
    for task in [&failed, &never] {
        queue.cancel_task(&task.id).unwrap();
        wait_state(&queue, &task.id, TransferState::Cancelled).await;
        assert_eq!(queue.resume_state(&task.id).unwrap(), None);
    }
}

#[tokio::test(start_paused = true)]
async fn a_failed_cleanup_is_repeated_when_the_queue_is_opened_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transfers.json");
    let runner = ChunkRunner::new(100, 10, Duration::from_millis(100)).shared();
    runner.fail_cleanup.store(true, SEQ);
    let options = QueueOptions::persistent(&path);
    let queue = open(&runner, settings(1, 0), options.clone()).await;
    let a = queue.add_task(upload("acct", "a")).unwrap();
    let b = queue.add_task(upload("acct", "b")).unwrap();
    wait_until(|| runner.processed("a").len() >= 3).await;
    queue.cancel_task(&a.id).unwrap();
    queue.cancel_task(&b.id).unwrap();
    wait_state(&queue, &a.id, TransferState::Failed).await;
    wait_state(&queue, &b.id, TransferState::Failed).await;
    let kept = queue.resume_state(&a.id).unwrap();
    assert!(kept.is_some());
    queue.shutdown().await.unwrap();
    drop(queue);

    // What the file keeps: failed tasks with their resume state and the
    // cancellation that is still owed.
    let saved = store::load(&path).unwrap();
    assert!(saved.iter().all(|s| s.task.state == TransferState::Failed));
    assert!(saved.iter().all(|s| s.cancel_pending));
    assert_eq!(saved[0].resume, kept);

    // The next open runs the cleanups again, without being asked, and the
    // tasks cannot be resumed meanwhile.
    let runner = ChunkRunner::new(100, 10, Duration::from_millis(100)).shared();
    let queue = open(&runner, settings(1, 0), options).await;
    wait_state(&queue, &a.id, TransferState::Cancelled).await;
    wait_state(&queue, &b.id, TransferState::Cancelled).await;
    assert_eq!(runner.cleanups_of("a"), vec![kept]);
    assert_eq!(runner.cleanups_of("b"), vec![None]);
    assert!(runner.starts().is_empty(), "nothing was resumed");
    queue.flush().await.unwrap();
    assert!(
        store::load(&path)
            .unwrap()
            .iter()
            .all(|s| s.task.state == TransferState::Cancelled && !s.cancel_pending)
    );
}

/// Sets a resume state the way a runner records a multipart upload that was
/// just created, optionally asking for it to be saved at once.
struct UploadIdRunner {
    urgent: bool,
}

impl TaskRunner for UploadIdRunner {
    fn run(&self, ctx: TaskContext) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            ctx.resume().set(json!({"upload_id": "u-1"}));
            if self.urgent {
                ctx.resume().persist_now();
            }
            ctx.stop().wait().await;
            ctx.stop().check()
        })
    }
}

#[tokio::test(start_paused = true)]
async fn an_urgent_resume_state_is_saved_without_waiting_for_the_persist_delay() {
    for urgent in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transfers.json");
        let options = QueueOptions {
            persist_delay: Duration::from_secs(3600),
            ..QueueOptions::persistent(&path)
        };
        let runner: Arc<dyn TaskRunner> = Arc::new(UploadIdRunner { urgent });
        let queue = open_with(runner, settings(1, 0), options).await;
        let task = queue.add_task(upload("acct", "a")).unwrap();
        wait_state(&queue, &task.id, TransferState::Running).await;
        let saved_with_the_id = || {
            store::load(&path).is_ok_and(|tasks| {
                tasks
                    .iter()
                    .any(|stored| stored.resume == Some(json!({"upload_id": "u-1"})))
            })
        };
        if urgent {
            // Far inside the persist delay (an hour), the file has the ID.
            let began = Instant::now();
            wait_until(saved_with_the_id).await;
            assert!(began.elapsed() < Duration::from_secs(60));
        } else {
            tokio::time::sleep(Duration::from_secs(30)).await;
            assert!(!saved_with_the_id(), "saved before the persist delay");
        }
        assert_eq!(
            queue.resume_state(&task.id).unwrap(),
            Some(json!({"upload_id": "u-1"}))
        );
        queue.shutdown().await.unwrap();
        // Shutdown saves whatever is left.
        assert!(saved_with_the_id());
    }
}

#[tokio::test(start_paused = true)]
async fn retryable_failures_back_off_continue_and_finally_fail() {
    let runner = ChunkRunner::new(5, 100, Duration::from_millis(100)).shared();
    let failure = Plan::FailAt(2, ErrorKind::Remote);
    runner.plan("a", &[failure, failure, failure]);
    let backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(30));
    let queue = open(
        &runner,
        settings(1, 2),
        QueueOptions {
            backoff,
            jitter_seed: Some(11),
            ..QueueOptions::default()
        },
    )
    .await;
    let mut random = SplitMix64::new(11);
    let expected = [backoff.delay(1, &mut random), backoff.delay(2, &mut random)];
    let mut events = queue.subscribe();
    let a = queue.add_task(upload("acct", "a")).unwrap();
    let b = queue.add_task(upload("acct", "b")).unwrap();
    let mut previous: Option<TransferTask> = None;
    let mut failed_at = None;
    let mut gaps = Vec::new();
    loop {
        let (at, event) = next_event(&mut events).await;
        let QueueEvent::Updated(task) = event else {
            continue;
        };
        if task.id != a.id {
            continue;
        }
        if let Some(before) = &previous {
            if task.state == TransferState::Running
                && before.error.is_none()
                && task.error.is_some()
            {
                assert!(
                    task.error
                        .as_deref()
                        .unwrap_or_default()
                        .contains("injected failure")
                );
                failed_at = Some(at);
            }
            if before.attempts > 0 && task.attempts > before.attempts {
                assert_eq!(task.error, None, "a new attempt clears the error");
                gaps.push(at - failed_at.take().expect("a retry follows a failure"));
            }
        }
        if task.state == TransferState::Failed {
            break;
        }
        previous = Some(task);
    }
    assert_eq!(gaps.len(), 2);
    for (gap, expected) in gaps.into_iter().zip(expected) {
        assert_close(gap, expected, Duration::from_millis(1));
    }
    let failed = queue.get(&a.id).unwrap();
    assert_eq!(failed.attempts, 3, "one run plus two automatic retries");
    assert_eq!(failed.error.as_deref(), Some("injected failure at chunk 2"));
    assert_eq!(
        runner.processed("a"),
        [0, 1],
        "retries continued instead of restarting"
    );
    assert_eq!(queue.resume_state(&a.id).unwrap(), Some(json!({"next": 2})));
    // The task kept its slot while backing off, so b ran only afterwards.
    wait_state(&queue, &b.id, TransferState::Completed).await;
    assert_eq!(runner.starts(), ["a", "a", "a", "b"]);
}

#[tokio::test(start_paused = true)]
async fn non_retryable_failures_fail_at_once() {
    let runner = ChunkRunner::new(5, 100, Duration::from_millis(100)).shared();
    runner.plan("a", &[Plan::FailAt(1, ErrorKind::Auth)]);
    let queue = open(&runner, settings(1, 3), options()).await;
    let a = queue.add_task(upload("acct", "a")).unwrap();
    let failed = wait_state(&queue, &a.id, TransferState::Failed).await;
    assert_eq!(failed.attempts, 1);
    assert_eq!(failed.error.as_deref(), Some("injected failure at chunk 1"));
    assert_eq!(runner.starts(), ["a"]);
}

#[tokio::test(start_paused = true)]
async fn manual_retry_requeues_with_a_fresh_budget_and_continues() {
    let runner = ChunkRunner::new(5, 100, Duration::from_millis(100)).shared();
    let failure = Plan::FailAt(3, ErrorKind::Remote);
    runner.plan("a", &[failure, failure, failure]);
    let backoff = Backoff::new(Duration::from_millis(100), Duration::from_millis(100));
    let queue = open(
        &runner,
        settings(1, 1),
        QueueOptions {
            backoff,
            ..options()
        },
    )
    .await;
    let a = queue.add_task(upload("acct", "a")).unwrap();
    let failed = wait_state(&queue, &a.id, TransferState::Failed).await;
    assert_eq!(failed.attempts, 2, "one automatic retry");
    queue.retry_task(&a.id).unwrap();
    let requeued = queue.get(&a.id).unwrap();
    assert!(matches!(
        requeued.state,
        TransferState::Queued | TransferState::Running
    ));
    assert_eq!(requeued.error, None);
    // Run 3 fails again, the fresh budget allows run 4, which succeeds.
    let done = wait_state(&queue, &a.id, TransferState::Completed).await;
    assert_eq!(done.attempts, 4);
    assert_eq!(
        runner.processed("a"),
        (0..5).collect::<Vec<_>>(),
        "no chunk twice"
    );
}

#[tokio::test(start_paused = true)]
async fn retry_budget_changes_apply_to_the_next_failure() {
    let runner = ChunkRunner::new(5, 100, Duration::from_millis(100)).shared();
    let failure = Plan::FailAt(1, ErrorKind::Io);
    runner.plan("a", &[failure, failure]);
    let backoff = Backoff::new(Duration::from_millis(50), Duration::from_millis(50));
    let queue = open(
        &runner,
        settings(1, 0),
        QueueOptions {
            backoff,
            ..options()
        },
    )
    .await;
    let a = queue.add_task(upload("acct", "a")).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    queue.update_settings(settings(1, 2)).unwrap();
    let done = wait_state(&queue, &a.id, TransferState::Completed).await;
    assert_eq!(done.attempts, 3);
}

#[tokio::test(start_paused = true)]
async fn pausing_during_backoff_stops_at_once() {
    let runner = ChunkRunner::new(5, 100, Duration::from_millis(100)).shared();
    runner.plan("a", &[Plan::FailAt(0, ErrorKind::Remote)]);
    let backoff = Backoff::new(Duration::from_secs(60), Duration::from_secs(60));
    let options = QueueOptions {
        backoff,
        jitter_seed: Some(7),
        ..QueueOptions::default()
    };
    let delay = backoff.delay(1, &mut SplitMix64::new(7));
    assert!(
        delay > Duration::from_secs(1),
        "the seed must give a noticeable backoff"
    );
    let queue = open(&runner, settings(1, 3), options).await;
    let a = queue.add_task(upload("acct", "a")).unwrap();
    wait_until(|| queue.get(&a.id).unwrap().error.is_some()).await;
    assert_eq!(
        queue.get(&a.id).unwrap().state,
        TransferState::Running,
        "backing off"
    );
    let paused_at = Instant::now();
    queue.pause_task(&a.id).unwrap();
    let stopped = wait_state(&queue, &a.id, TransferState::Stopped).await;
    assert!(paused_at.elapsed() <= Duration::from_millis(2));
    assert_eq!(stopped.error, None);
    assert_eq!(stopped.attempts, 1);
}

#[tokio::test(start_paused = true)]
async fn cancelling_during_backoff_cleans_up_once() {
    let runner = ChunkRunner::new(5, 100, Duration::from_millis(100)).shared();
    runner.plan("a", &[Plan::FailAt(2, ErrorKind::Remote)]);
    let backoff = Backoff::new(Duration::from_secs(60), Duration::from_secs(60));
    let delay = backoff.delay(1, &mut SplitMix64::new(7));
    assert!(
        delay > Duration::from_secs(1),
        "the seed must give a noticeable backoff"
    );
    let queue = open(
        &runner,
        settings(1, 3),
        QueueOptions {
            backoff,
            ..options()
        },
    )
    .await;
    let a = queue.add_task(upload("acct", "a")).unwrap();
    wait_until(|| queue.get(&a.id).unwrap().error.is_some()).await;
    queue.cancel_task(&a.id).unwrap();
    let cancelled = wait_state(&queue, &a.id, TransferState::Cancelled).await;
    assert_eq!(cancelled.attempts, 1, "no further attempt after the cancel");
    assert_eq!(cancelled.error, None);
    assert_eq!(runner.cleanups_of("a"), vec![Some(json!({"next": 2}))]);
}

#[tokio::test(start_paused = true)]
async fn invalid_transitions_are_conflicts() {
    let runner = ChunkRunner::new(3, 10, Duration::from_millis(100)).shared();
    runner.plan("failing", &[Plan::FailAt(0, ErrorKind::Auth)]);
    runner.plan("stubborn", &[Plan::Hang]);
    let queue = open(
        &runner,
        settings(4, 0),
        QueueOptions {
            stop_grace: Duration::from_secs(10),
            ..options()
        },
    )
    .await;
    let done = queue.add_task(upload("acct", "done")).unwrap();
    let failing = queue.add_task(upload("acct", "failing")).unwrap();
    let stubborn = queue.add_task(upload("acct", "stubborn")).unwrap();
    wait_state(&queue, &done.id, TransferState::Completed).await;
    wait_state(&queue, &failing.id, TransferState::Failed).await;

    assert_conflict(
        queue.pause_task(&done.id),
        "cannot pause a task that is completed",
    );
    assert_conflict(
        queue.resume_task(&done.id),
        "cannot resume a task that is completed",
    );
    assert_conflict(
        queue.retry_task(&done.id),
        "cannot retry a task that is completed",
    );
    assert_conflict(
        queue.cancel_task(&done.id),
        "cannot cancel a task that is completed",
    );
    assert_conflict(
        queue.pause_task(&failing.id),
        "cannot pause a task that is failed",
    );
    assert_conflict(
        queue.resume_task(&failing.id),
        "cannot resume a task that is failed",
    );

    assert_conflict(
        queue.resume_task(&stubborn.id),
        "cannot resume a task that is running",
    );
    assert_conflict(
        queue.retry_task(&stubborn.id),
        "cannot retry a task that is running",
    );
    queue.pause_task(&stubborn.id).unwrap();
    queue.pause_task(&stubborn.id).unwrap();
    assert_conflict(
        queue.resume_task(&stubborn.id),
        "cannot resume a task that is stopping",
    );
    queue.cancel_task(&stubborn.id).unwrap();
    assert_conflict(
        queue.pause_task(&stubborn.id),
        "cannot pause a task that is being cancelled",
    );
    queue.cancel_task(&stubborn.id).unwrap();
    wait_state(&queue, &stubborn.id, TransferState::Cancelled).await;
    for (result, message) in [
        (
            queue.pause_task(&stubborn.id),
            "cannot pause a task that is cancelled",
        ),
        (
            queue.resume_task(&stubborn.id),
            "cannot resume a task that is cancelled",
        ),
        (
            queue.retry_task(&stubborn.id),
            "cannot retry a task that is cancelled",
        ),
        (
            queue.cancel_task(&stubborn.id),
            "cannot cancel a task that is cancelled",
        ),
    ] {
        assert_conflict(result, message);
    }
    assert_eq!(runner.cleanups_of("stubborn").len(), 1);
    assert_eq!(
        runner.dropped.load(SEQ),
        1,
        "the hanging run was dropped after the grace period"
    );

    let paused = queue.add_task(upload("acct", "paused")).unwrap();
    queue.pause_task(&paused.id).unwrap();
    wait_state(&queue, &paused.id, TransferState::Stopped).await;
    assert_conflict(
        queue.retry_task(&paused.id),
        "cannot retry a task that is stopped",
    );
    assert_conflict(
        queue.pause_task(&paused.id),
        "cannot pause a task that is stopped",
    );
    // A failed task can be cancelled, and its cleanup runs (a failed upload
    // may still have an open multipart upload).
    queue.cancel_task(&failing.id).unwrap();
    wait_state(&queue, &failing.id, TransferState::Cancelled).await;
    assert_eq!(runner.cleanups_of("failing").len(), 1);

    for result in [
        queue.pause_task("nope"),
        queue.resume_task("nope"),
        queue.cancel_task("nope"),
        queue.retry_task("nope"),
    ] {
        assert_eq!(result.unwrap_err().kind(), ErrorKind::NotFound);
    }
}

#[tokio::test(start_paused = true)]
async fn bulk_commands_follow_the_queue_rules() {
    let runner = ChunkRunner::new(10, 10, Duration::from_millis(100)).shared();
    let queue = open(&runner, settings(1, 0), options()).await;
    let specs: Vec<NewTask> = (0..6).map(|i| upload("acct", &format!("t{i}"))).collect();
    let ids: Vec<String> = queue
        .add_tasks(specs)
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    wait_until(|| runner.processed("t0").len() >= 3).await;

    queue.stop_all().unwrap();
    let states: Vec<TransferState> = queue.list().into_iter().map(|t| t.state).collect();
    assert_eq!(
        states[0],
        TransferState::Running,
        "running tasks stop when their runner returns"
    );
    assert_eq!(
        states[1..],
        [TransferState::Stopped; 5],
        "queued tasks stop at once"
    );
    wait_until(|| {
        queue
            .list()
            .iter()
            .all(|t| t.state == TransferState::Stopped)
    })
    .await;
    let starts = runner.starts().len();
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(
        runner.starts().len(),
        starts,
        "nothing starts while stopped"
    );

    queue.start_all().unwrap();
    assert!(
        queue
            .list()
            .iter()
            .all(|t| matches!(t.state, TransferState::Queued | TransferState::Running))
    );
    wait_state(&queue, &ids[0], TransferState::Completed).await;

    queue.cancel_all().unwrap();
    wait_until(|| {
        queue
            .list()
            .iter()
            .all(|t| matches!(t.state, TransferState::Completed | TransferState::Cancelled))
    })
    .await;
    assert_eq!(queue.get(&ids[0]).unwrap().state, TransferState::Completed);
    for id in &ids[1..] {
        assert_eq!(queue.get(id).unwrap().state, TransferState::Cancelled);
    }
    assert_eq!(
        runner.cleanups.lock().unwrap().len(),
        5,
        "one cleanup per cancelled task"
    );

    let mut events = queue.subscribe();
    assert_eq!(queue.clear_finished().unwrap(), ids);
    match events.try_recv() {
        Ok(QueueEvent::Removed(removed)) => assert_eq!(removed, ids),
        other => panic!("expected one removal event, got {other:?}"),
    }
    assert!(queue.list().is_empty());
    assert!(queue.clear_finished().unwrap().is_empty());
    assert!(matches!(events.try_recv(), Err(TryRecvError::Empty)));
}

#[tokio::test(start_paused = true)]
async fn clear_finished_keeps_unfinished_tasks() {
    let runner = ChunkRunner::new(2, 10, Duration::from_millis(100)).shared();
    runner.plan("failed", &[Plan::FailAt(0, ErrorKind::Auth)]);
    let queue = open(&runner, settings(1, 0), options()).await;
    let failed = queue.add_task(upload("acct", "failed")).unwrap();
    let done = queue.add_task(upload("acct", "done")).unwrap();
    let running = queue.add_task(upload("acct", "running")).unwrap();
    let stopped = queue.add_task(upload("acct", "stopped")).unwrap();
    queue.pause_task(&stopped.id).unwrap();
    wait_state(&queue, &done.id, TransferState::Completed).await;
    wait_state(&queue, &running.id, TransferState::Running).await;
    assert_eq!(queue.clear_finished().unwrap(), vec![done.id.clone()]);
    let left: Vec<String> = queue.list().into_iter().map(|t| t.id).collect();
    assert_eq!(left, vec![failed.id, running.id, stopped.id]);
}

#[tokio::test(start_paused = true)]
async fn progress_events_are_throttled_and_carry_speed_and_eta() {
    let runner = ChunkRunner::new(300, 1000, Duration::from_millis(10)).shared();
    let queue = open(&runner, settings(1, 0), options()).await;
    let mut events = queue.subscribe();
    let start = Instant::now();
    let a = queue.add_task(upload("acct", "a")).unwrap();
    let mut started = false;
    let mut progress = Vec::new();
    let finished_at = loop {
        let (at, event) = next_event(&mut events).await;
        let QueueEvent::Updated(task) = event else {
            continue;
        };
        assert_eq!(task.id, a.id);
        match task.state {
            TransferState::Queued => {}
            TransferState::Running if !started => started = true,
            TransferState::Running => progress.push((at, task)),
            TransferState::Completed => {
                assert_eq!((task.bytes_done, task.speed, task.eta), (300_000, 0, None));
                break at;
            }
            other => panic!("unexpected state {other:?}"),
        }
    };
    // The completion was sent the moment the runner returned.
    assert_eq!(finished_at - start, Duration::from_secs(3));
    // Ticks at 250 ms, 500 ms, ..., 2750 ms.
    assert_eq!(progress.len(), 11);
    for pair in progress.windows(2) {
        assert!(pair[1].0 - pair[0].0 >= Duration::from_millis(250));
        assert!(pair[1].1.bytes_done > pair[0].1.bytes_done);
    }
    for (at, task) in &progress {
        let elapsed = (*at - start).as_millis() as u64;
        assert_eq!(task.bytes_done, elapsed * 100, "1000 bytes every 10 ms");
        assert_eq!(task.bytes_total, Some(300_000));
        assert_eq!(task.speed, 100_000);
        assert_eq!(
            task.eta,
            Some((300_000 - task.bytes_done).div_ceil(100_000))
        );
    }
}

#[tokio::test(start_paused = true)]
async fn speed_drops_to_zero_while_a_run_is_idle() {
    let runner = ChunkRunner::new(2, 50_000, Duration::from_secs(5)).shared();
    let queue = open(&runner, settings(1, 0), options()).await;
    let a = queue.add_task(upload("acct", "a")).unwrap();
    // First chunk lands at 5 s; then nothing moves for another 5 s.
    tokio::time::sleep(Duration::from_millis(5300)).await;
    let moving = queue.get(&a.id).unwrap();
    assert_eq!(moving.bytes_done, 50_000);
    assert!(moving.speed > 0);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let idle = queue.get(&a.id).unwrap();
    assert_eq!((idle.speed, idle.eta), (0, None));
}

#[tokio::test(start_paused = true)]
async fn shared_bandwidth_limit_caps_the_aggregate_rate() {
    let runner: Arc<dyn TaskRunner> = Arc::new(ThrottleRunner {
        bytes: 300 * 1024,
        piece: 16 * 1024,
    });
    let limited = TransferSettings {
        bandwidth_limit_kibps: Some(100),
        ..settings(3, 0)
    };
    let queue = open_with(runner, limited, options()).await;
    let start = Instant::now();
    let tasks = queue
        .add_tasks(vec![upload("a", "a"), upload("b", "b"), upload("c", "c")])
        .unwrap();
    let mut finished = Vec::new();
    for task in &tasks {
        wait_state(&queue, &task.id, TransferState::Completed).await;
        finished.push(start.elapsed());
    }
    // 900 KiB at 100 KiB/s, minus the initial burst of 12.5 KiB (1/8 s).
    let expected = Duration::from_secs_f64((900.0 * 1024.0 - 12_800.0) / 102_400.0);
    let last = finished.iter().copied().max().unwrap_or_default();
    assert_close(last, expected, expected / 50);
    // Pieces are served first in first out, so the tasks finish together.
    let first = finished.iter().copied().min().unwrap_or_default();
    assert!(last - first <= Duration::from_millis(500), "{finished:?}");
}

#[tokio::test(start_paused = true)]
async fn bandwidth_limit_changes_apply_to_running_tasks() {
    let runner: Arc<dyn TaskRunner> = Arc::new(ThrottleRunner {
        bytes: 300 * 1024,
        piece: 16 * 1024,
    });
    let limited = TransferSettings {
        bandwidth_limit_kibps: Some(100),
        ..settings(3, 0)
    };
    let queue = open_with(runner, limited.clone(), options()).await;
    assert_eq!(queue.limiter().rate(), NonZeroU64::new(102_400));
    let start = Instant::now();
    let tasks = queue
        .add_tasks(vec![upload("a", "a"), upload("b", "b"), upload("c", "c")])
        .unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let moved: u64 = queue.list().iter().map(|t| t.bytes_done).sum();
    // About 12.5 KiB of burst plus 2 s at 100 KiB/s, give or take one piece
    // per task and one progress interval.
    assert!((180_000..=240_000).contains(&moved), "moved {moved}");
    queue
        .update_settings(TransferSettings {
            bandwidth_limit_kibps: Some(200),
            ..limited
        })
        .unwrap();
    assert_eq!(queue.limiter().rate(), NonZeroU64::new(204_800));
    for task in &tasks {
        wait_state(&queue, &task.id, TransferState::Completed).await;
    }
    // 2 s at 100 KiB/s, the rest of 900 KiB at 200 KiB/s.
    let at_switch = 12_800.0 + 2.0 * 102_400.0;
    let expected = Duration::from_secs_f64(2.0 + (921_600.0 - at_switch) / 204_800.0);
    assert_close(start.elapsed(), expected, expected / 50);

    queue.update_settings(settings(3, 0)).unwrap();
    assert_eq!(queue.limiter().rate(), None);
}

#[tokio::test(start_paused = true)]
async fn queue_survives_a_restart_and_resumes_from_saved_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data").join("transfers.json");
    let first_runner = ChunkRunner::new(10, 100, Duration::from_millis(100)).shared();
    let first = open(
        &first_runner,
        settings(1, 0),
        QueueOptions::persistent(&path),
    )
    .await;
    let a = first.add_task(upload("acct", "a")).unwrap();
    let b = first.add_task(upload("acct", "b")).unwrap();
    wait_until(|| first_runner.processed("a").len() >= 4).await;
    first.shutdown().await.unwrap();
    let next = first_runner.processed("a").len() as u64;
    assert_eq!(first.get(&a.id).unwrap().state, TransferState::Stopped);
    assert_eq!(first.get(&b.id).unwrap().state, TransferState::Stopped);
    drop(first);

    let raw: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(raw["version"], json!(1));
    assert_eq!(raw["tasks"].as_array().map(Vec::len), Some(2));
    assert_eq!(raw["tasks"][0]["task"]["id"], json!(a.id));
    assert_eq!(raw["tasks"][0]["task"]["state"], json!("stopped"));
    assert_eq!(raw["tasks"][0]["resume"], json!({"next": next}));
    assert_eq!(raw["tasks"][1]["resume"], Value::Null);

    let second_runner = ChunkRunner::new(10, 100, Duration::from_millis(100)).shared();
    let second = open(
        &second_runner,
        settings(1, 0),
        QueueOptions::persistent(&path),
    )
    .await;
    let listed = second.list();
    assert_eq!(
        listed
            .iter()
            .map(|t| (t.id.clone(), t.state))
            .collect::<Vec<_>>(),
        vec![
            (a.id.clone(), TransferState::Stopped),
            (b.id.clone(), TransferState::Stopped),
        ]
    );
    assert_eq!(listed[0].bytes_done, next * 100);
    assert_eq!(
        second.resume_state(&a.id).unwrap(),
        Some(json!({"next": next}))
    );
    second.start_all().unwrap();
    let done = wait_state(&second, &a.id, TransferState::Completed).await;
    wait_state(&second, &b.id, TransferState::Completed).await;
    assert_eq!(second_runner.processed("a"), (next..10).collect::<Vec<_>>());
    assert_eq!(second_runner.processed("b"), (0..10).collect::<Vec<_>>());
    assert_eq!(done.attempts, 2);
    second.shutdown().await.unwrap();
    let reloaded = store::load(&path).unwrap();
    assert!(
        reloaded
            .iter()
            .all(|s| s.task.state == TransferState::Completed && s.resume.is_none())
    );
}

#[tokio::test(start_paused = true)]
async fn loading_stops_unfinished_tasks_and_fails_tasks_without_runner() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transfers.json");
    store::save(
        &path,
        &[
            record("queued", TransferKind::Upload, TransferState::Queued),
            // Saved while backing off: running, with the error being retried.
            {
                let mut stored = record("running", TransferKind::Upload, TransferState::Running);
                stored.task.error = Some("retrying after a timeout".to_owned());
                stored
            },
            record("failed", TransferKind::Upload, TransferState::Failed),
            record("completed", TransferKind::Upload, TransferState::Completed),
            record("delete", TransferKind::Delete, TransferState::Queued),
        ],
    )
    .unwrap();
    let runner = ChunkRunner::new(3, 100, Duration::from_millis(10)).shared();
    let runners = Runners::new().with(
        TransferKind::Upload,
        Arc::clone(&runner) as Arc<dyn TaskRunner>,
    );
    let queue = TransferQueue::open(settings(4, 0), runners, QueueOptions::persistent(&path))
        .await
        .unwrap();
    let listed = queue.list();
    let states: Vec<(String, TransferState)> =
        listed.iter().map(|t| (t.id.clone(), t.state)).collect();
    assert_eq!(
        states,
        vec![
            ("queued".to_owned(), TransferState::Stopped),
            ("running".to_owned(), TransferState::Stopped),
            ("failed".to_owned(), TransferState::Failed),
            ("completed".to_owned(), TransferState::Completed),
            ("delete".to_owned(), TransferState::Stopped),
        ]
    );
    assert!(listed.iter().all(|t| t.speed == 0 && t.eta.is_none()));
    assert_eq!(
        listed[1].error, None,
        "a stale retry error is dropped on load"
    );
    assert_eq!(listed[2].error.as_deref(), Some("earlier failure"));
    assert!(queue.list().iter().all(|t| t.bytes_done == 100));
    queue.start_all().unwrap();
    let delete = wait_state(&queue, "delete", TransferState::Failed).await;
    assert_eq!(
        delete.error.as_deref(),
        Some("no runner is registered for delete tasks")
    );
    wait_state(&queue, "queued", TransferState::Completed).await;
    wait_state(&queue, "running", TransferState::Completed).await;
    // Both resumed from the saved checkpoint {"next": 1}.
    assert_eq!(runner.processed("queued"), [1, 2]);
    assert_eq!(runner.processed("running"), [1, 2]);
}

#[tokio::test(start_paused = true)]
async fn damaged_queue_file_is_reported_and_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transfers.json");
    let runner = ChunkRunner::new(1, 1, Duration::from_millis(10)).shared();
    for contents in [
        &b"{\"version\": 1, \"tasks\": [oops"[..],
        b"{\"version\": 99, \"tasks\": []}",
    ] {
        fs::write(&path, contents).unwrap();
        let error = TransferQueue::open(
            settings(1, 0),
            Runners::for_all(Arc::clone(&runner) as Arc<dyn TaskRunner>),
            QueueOptions::persistent(&path),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Io);
        assert!(error.to_string().contains("transfers.json"), "{error}");
        assert!(error.to_string().contains("left unchanged"), "{error}");
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(fs::read(&path).unwrap(), contents);
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no stray files"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn saves_are_delayed_coalesced_and_skip_progress_ticks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transfers.json");
    let runner = ChunkRunner::new(1_000_000, 10, Duration::from_millis(10))
        .without_checkpoints()
        .shared();
    let options = QueueOptions {
        persist_delay: Duration::from_secs(1),
        ..QueueOptions::persistent(&path)
    };
    let queue = open(&runner, settings(1, 0), options).await;
    let mut events = queue.subscribe();
    let tasks = queue
        .add_tasks(vec![
            upload("acct", "a"),
            upload("acct", "b"),
            upload("acct", "c"),
        ])
        .unwrap();
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(!path.exists());
    assert_eq!(queue.persisted_writes(), 0);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(path.exists());
    assert_eq!(queue.persisted_writes(), 1);

    // Several changes inside one delay share one write.
    queue.pause_task(&tasks[1].id).unwrap();
    queue.pause_task(&tasks[2].id).unwrap();
    queue.resume_task(&tasks[2].id).unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(queue.persisted_writes(), 2);

    // Progress alone (task a keeps moving) never writes the file.
    drain(&mut events);
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(queue.persisted_writes(), 2);
    assert!(
        drain(&mut events).len() >= 19,
        "progress events kept flowing"
    );

    // flush writes pending changes at once, and nothing when clean.
    queue.pause_task(&tasks[2].id).unwrap();
    queue.flush().await.unwrap();
    assert_eq!(queue.persisted_writes(), 3);
    queue.flush().await.unwrap();
    assert_eq!(queue.persisted_writes(), 3);
    let saved = store::load(&path).unwrap();
    assert_eq!(saved[2].task.state, TransferState::Stopped);
}

#[tokio::test(start_paused = true)]
async fn persistence_failures_are_reported_once_and_retried() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sub").join("transfers.json");
    let runner = ChunkRunner::new(3, 10, Duration::from_millis(100)).shared();
    let options = QueueOptions {
        persist_delay: Duration::from_millis(100),
        ..QueueOptions::persistent(&path)
    };
    let queue = open(&runner, settings(1, 0), options).await;
    // A file where the queue's directory should be makes every save fail.
    fs::write(dir.path().join("sub"), b"in the way").unwrap();
    let mut events = queue.subscribe();
    let tasks = queue
        .add_tasks(vec![upload("acct", "a"), upload("acct", "b")])
        .unwrap();
    queue.pause_task(&tasks[1].id).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    queue.resume_task(&tasks[1].id).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let failures: Vec<String> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            QueueEvent::PersistFailed(message) => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].contains("cannot save the transfer queue"));
    let error = queue.flush().await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Io);

    fs::remove_file(dir.path().join("sub")).unwrap();
    queue.flush().await.unwrap();
    assert_eq!(store::load(&path).unwrap().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn shutdown_stops_runs_saves_and_refuses_commands() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transfers.json");
    let runner = ChunkRunner::new(10, 100, Duration::from_millis(100)).shared();
    let queue = open(&runner, settings(1, 0), QueueOptions::persistent(&path)).await;
    let a = queue.add_task(upload("acct", "a")).unwrap();
    let b = queue.add_task(upload("acct", "b")).unwrap();
    wait_until(|| runner.processed("a").len() >= 2).await;
    let started = Instant::now();
    queue.shutdown().await.unwrap();
    assert!(
        started.elapsed() <= Duration::from_millis(5),
        "cooperative runners stop at once"
    );
    assert_eq!(queue.get(&a.id).unwrap().state, TransferState::Stopped);
    assert_eq!(queue.get(&b.id).unwrap().state, TransferState::Stopped);
    assert_eq!(queue.resume_state(&a.id).unwrap(), Some(json!({"next": 2})));
    assert_eq!(runner.dropped.load(SEQ), 0);

    let refused = queue.add_task(upload("acct", "c")).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Conflict);
    assert_conflict(queue.resume_task(&a.id), "shut down");
    assert_conflict(queue.start_all(), "shut down");
    assert_conflict(queue.update_settings(settings(2, 0)), "shut down");
    assert_eq!(queue.list().len(), 2);
    queue.shutdown().await.unwrap();

    let saved = store::load(&path).unwrap();
    assert!(saved.iter().all(|s| s.task.state == TransferState::Stopped));
    assert_eq!(saved[0].resume, Some(json!({"next": 2})));
}

#[tokio::test(start_paused = true)]
async fn runners_ignoring_stop_are_dropped_after_the_grace_period() {
    let runner = ChunkRunner::new(10, 100, Duration::from_millis(100)).shared();
    runner.plan("paused", &[Plan::Hang]);
    runner.plan("at-shutdown", &[Plan::Hang]);
    let options = QueueOptions {
        stop_grace: Duration::from_secs(2),
        ..options()
    };
    let queue = open(&runner, settings(2, 0), options).await;
    let paused = queue.add_task(upload("acct", "paused")).unwrap();
    let at_shutdown = queue.add_task(upload("acct", "at-shutdown")).unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    let asked = Instant::now();
    queue.pause_task(&paused.id).unwrap();
    tokio::time::sleep(Duration::from_millis(1990)).await;
    assert_eq!(queue.get(&paused.id).unwrap().state, TransferState::Running);
    wait_state(&queue, &paused.id, TransferState::Stopped).await;
    assert_close(
        asked.elapsed(),
        Duration::from_secs(2),
        Duration::from_millis(2),
    );
    assert_eq!(runner.dropped.load(SEQ), 1);

    let asked = Instant::now();
    queue.shutdown().await.unwrap();
    assert_close(
        asked.elapsed(),
        Duration::from_secs(2),
        Duration::from_millis(2),
    );
    assert_eq!(
        queue.get(&at_shutdown.id).unwrap().state,
        TransferState::Stopped
    );
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(runner.dropped.load(SEQ), 2);
    assert_eq!(runner.active.load(SEQ), 0);
}

#[tokio::test(start_paused = true)]
async fn cancellations_cut_short_by_shutdown_finish_on_the_next_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transfers.json");
    let runner = ChunkRunner::new(100, 10, Duration::from_millis(100)).shared();
    runner.hang_cleanup.store(true, SEQ);
    let options = QueueOptions {
        stop_grace: Duration::from_secs(1),
        ..QueueOptions::persistent(&path)
    };
    let queue = open(&runner, settings(1, 0), options.clone()).await;
    let a = queue.add_task(upload("acct", "a")).unwrap();
    let b = queue.add_task(upload("acct", "b")).unwrap();
    wait_until(|| runner.processed("a").len() >= 3).await;
    // Both cleanups hang: the running task stays `running`, the queued one
    // keeps its state and is not schedulable.
    queue.cancel_task(&a.id).unwrap();
    queue.cancel_task(&b.id).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(queue.get(&a.id).unwrap().state, TransferState::Running);
    assert_eq!(queue.get(&b.id).unwrap().state, TransferState::Queued);
    queue.cancel_task(&b.id).unwrap();
    assert_conflict(queue.pause_task(&b.id), "being cancelled");
    assert_conflict(queue.resume_task(&b.id), "being cancelled");
    let asked = Instant::now();
    queue.shutdown().await.unwrap();
    assert_close(
        asked.elapsed(),
        Duration::from_secs(1),
        Duration::from_millis(2),
    );
    assert_eq!(queue.get(&a.id).unwrap().state, TransferState::Stopped);
    assert_eq!(queue.get(&b.id).unwrap().state, TransferState::Stopped);
    drop(queue);
    let saved = store::load(&path).unwrap();
    assert!(saved.iter().all(|stored| stored.cancel_pending));
    assert_eq!(saved[0].resume, Some(json!({"next": 3})));

    // The next open finishes both cancellations without being asked again,
    // and the tasks cannot be resumed meanwhile.
    let runner = ChunkRunner::new(100, 10, Duration::from_millis(100)).shared();
    runner.hang_cleanup.store(true, SEQ);
    let queue = open(&runner, settings(1, 0), options.clone()).await;
    assert_conflict(queue.resume_task(&a.id), "being cancelled");
    assert_conflict(
        queue.start_all().and(queue.resume_task(&b.id)),
        "being cancelled",
    );
    // Only one queue may use the file at a time: finish this one first.
    queue.shutdown().await.unwrap();
    drop(queue);

    let runner = ChunkRunner::new(100, 10, Duration::from_millis(100)).shared();
    let queue = open(&runner, settings(1, 0), options).await;
    wait_state(&queue, &a.id, TransferState::Cancelled).await;
    wait_state(&queue, &b.id, TransferState::Cancelled).await;
    assert_eq!(runner.cleanups_of("a"), vec![Some(json!({"next": 3}))]);
    assert_eq!(runner.cleanups_of("b"), vec![None]);
    assert!(runner.starts().is_empty(), "nothing was resumed");
    queue.flush().await.unwrap();
    assert!(
        store::load(&path)
            .unwrap()
            .iter()
            .all(|s| !s.cancel_pending)
    );
}

#[tokio::test(start_paused = true)]
async fn hanging_cleanups_time_out_and_free_their_slot() {
    let runner = ChunkRunner::new(100, 10, Duration::from_millis(100)).shared();
    runner.hang_cleanup.store(true, SEQ);
    let options = QueueOptions {
        cleanup_timeout: Duration::from_secs(2),
        ..options()
    };
    let queue = open(&runner, settings(1, 0), options).await;
    let running = queue.add_task(upload("acct", "running")).unwrap();
    let idle = queue.add_task(upload("acct", "idle")).unwrap();
    let next = queue.add_task(upload("acct", "next")).unwrap();
    wait_until(|| !runner.processed("running").is_empty()).await;
    let asked = Instant::now();
    queue.cancel_task(&running.id).unwrap();
    queue.cancel_task(&idle.id).unwrap();
    // A cleanup that does not finish counts as failed: the tasks are kept.
    for id in [&running.id, &idle.id] {
        wait_until(|| queue.get(id).unwrap().state == TransferState::Failed).await;
        let kept = queue.get(id).unwrap();
        assert_eq!(
            kept.error.as_deref(),
            Some(kept_message("the cleanup did not finish within 2s").as_str())
        );
    }
    assert_close(
        asked.elapsed(),
        Duration::from_secs(2),
        Duration::from_millis(2),
    );
    assert!(queue.resume_state(&running.id).unwrap().is_some());
    // The slot held by the running task's cleanup is free again.
    wait_state(&queue, &next.id, TransferState::Running).await;

    // Once the cleanup finishes in time, cancelling again succeeds.
    runner.hang_cleanup.store(false, SEQ);
    queue.cancel_task(&running.id).unwrap();
    wait_state(&queue, &running.id, TransferState::Cancelled).await;
    assert_eq!(queue.resume_state(&running.id).unwrap(), None);
}

#[tokio::test(start_paused = true)]
async fn a_stop_before_the_run_begins_never_starts_the_runner() {
    let runner = ChunkRunner::new(5, 10, Duration::from_millis(100)).shared();
    let queue = open(&runner, settings(2, 0), options()).await;
    let paused = queue.add_task(upload("acct", "paused")).unwrap();
    let cancelled = queue.add_task(upload("acct", "cancelled")).unwrap();
    // Both were started (slots taken) but their drivers have not run yet.
    queue.pause_task(&paused.id).unwrap();
    queue.cancel_task(&cancelled.id).unwrap();
    wait_state(&queue, &paused.id, TransferState::Stopped).await;
    wait_state(&queue, &cancelled.id, TransferState::Cancelled).await;
    assert!(runner.starts().is_empty(), "{:?}", runner.starts());
    assert_eq!(runner.cleanups_of("cancelled"), vec![None]);
}

#[tokio::test(start_paused = true)]
async fn extreme_durations_do_not_overflow() {
    let dir = tempfile::tempdir().unwrap();
    let runner = ChunkRunner::new(100, 10, Duration::from_millis(100)).shared();
    runner.plan("stubborn", &[Plan::Hang]);
    runner.plan("retrying", &[Plan::FailAt(0, ErrorKind::Remote)]);
    let options = QueueOptions {
        stop_grace: Duration::MAX,
        cleanup_timeout: Duration::MAX,
        persist_delay: Duration::MAX,
        backoff: Backoff::new(Duration::MAX, Duration::MAX),
        jitter_seed: Some(7),
        ..QueueOptions::persistent(dir.path().join("transfers.json"))
    };
    let queue = open(&runner, settings(3, 1), options).await;
    let polite = queue.add_task(upload("acct", "polite")).unwrap();
    let stubborn = queue.add_task(upload("acct", "stubborn")).unwrap();
    let retrying = queue.add_task(upload("acct", "retrying")).unwrap();
    wait_until(|| !runner.processed("polite").is_empty()).await;
    wait_until(|| queue.get(&retrying.id).unwrap().error.is_some()).await;
    queue.pause_task(&polite.id).unwrap();
    queue.pause_task(&stubborn.id).unwrap();
    queue.pause_task(&retrying.id).unwrap();
    wait_state(&queue, &polite.id, TransferState::Stopped).await;
    wait_state(&queue, &retrying.id, TransferState::Stopped).await;
    // With an unbounded grace the stubborn run keeps running until shutdown.
    tokio::time::sleep(Duration::from_secs(3600)).await;
    assert_eq!(
        queue.get(&stubborn.id).unwrap().state,
        TransferState::Running
    );
    queue.flush().await.unwrap();
}

#[test]
fn public_types_are_thread_safe() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<TransferQueue>();
    send_sync::<QueueEvent>();
    send_sync::<Runners>();
    send_sync::<TaskContext>();
    send_sync::<Progress>();
    send_sync::<StopSignal>();
    send_sync::<StopController>();
    send_sync::<ResumeState>();
    send_sync::<BandwidthLimiter>();
    send_sync::<crate::transfer::Acquire>();
    send_sync::<crate::transfer::ThrottledStream<futures::stream::Empty<Result<bytes::Bytes>>>>();
    send_sync::<crate::transfer::ThrottledRead<&'static [u8]>>();
}

#[tokio::test(start_paused = true)]
async fn api_futures_are_send() {
    fn send<T: Send>(_: &T) {}
    let dir = tempfile::tempdir().unwrap();
    let runner: Arc<dyn TaskRunner> = ChunkRunner::new(1, 1, Duration::from_millis(1)).shared();
    let opening = TransferQueue::open(
        settings(1, 0),
        Runners::for_all(runner),
        QueueOptions::persistent(dir.path().join("transfers.json")),
    );
    send(&opening);
    let queue = opening.await.unwrap();
    queue.add_task(upload("acct", "a")).unwrap();
    let flushing = queue.flush();
    send(&flushing);
    flushing.await.unwrap();
    let stopping = queue.shutdown();
    send(&stopping);
    stopping.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_panicking_runner_fails_its_task_and_frees_the_slot() {
    let runner = ChunkRunner::new(2, 10, Duration::from_millis(10)).shared();
    runner.plan("boom", &[Plan::Panic]);
    let queue = open(&runner, settings(1, 3), options()).await;
    let boom = queue.add_task(upload("acct", "boom")).unwrap();
    let next = queue.add_task(upload("acct", "next")).unwrap();
    let failed = wait_state(&queue, &boom.id, TransferState::Failed).await;
    let error = failed.error.unwrap_or_default();
    assert!(
        error.contains("the transfer runner panicked: injected panic"),
        "{error}"
    );
    assert_eq!(failed.attempts, 1, "internal errors are not retried");
    wait_state(&queue, &next.id, TransferState::Completed).await;
}

#[tokio::test(start_paused = true)]
async fn dropping_the_queue_stops_and_saves_in_the_background() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transfers.json");
    let runner = ChunkRunner::new(10, 100, Duration::from_millis(100)).shared();
    let queue = open(&runner, settings(1, 0), QueueOptions::persistent(&path)).await;
    let a = queue.add_task(upload("acct", "a")).unwrap();
    wait_until(|| runner.processed("a").len() >= 3).await;
    drop(queue);
    wait_until(|| {
        store::load(&path)
            .ok()
            .and_then(|tasks| tasks.into_iter().next())
            .is_some_and(|stored| stored.task.state == TransferState::Stopped)
    })
    .await;
    let saved = store::load(&path).unwrap();
    assert_eq!(saved[0].task.id, a.id);
    assert_eq!(saved[0].resume, Some(json!({"next": 3})));
    assert_eq!(runner.active.load(SEQ), 0);
}

#[tokio::test(start_paused = true)]
async fn settings_and_options_are_validated() {
    let runner: Arc<dyn TaskRunner> = ChunkRunner::new(1, 1, Duration::from_millis(1)).shared();
    for (bad, message) in [
        (settings(0, 0), "max_concurrent_tasks"),
        (
            TransferSettings {
                max_parts_per_task: 0,
                ..settings(1, 0)
            },
            "max_parts_per_task",
        ),
        (
            TransferSettings {
                bandwidth_limit_kibps: Some(0),
                ..settings(1, 0)
            },
            "bandwidth_limit_kibps",
        ),
    ] {
        let error = TransferQueue::open(
            bad.clone(),
            Runners::for_all(Arc::clone(&runner)),
            options(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Usage);
        assert!(error.to_string().contains(message), "{error}");
        let queue = open_with(Arc::clone(&runner), settings(2, 0), options()).await;
        let error = queue.update_settings(bad).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Usage);
        assert_eq!(
            queue.settings(),
            settings(2, 0),
            "rejected settings change nothing"
        );
    }
    for interval in [Duration::ZERO, Duration::from_secs(2 * 24 * 60 * 60)] {
        let bad_interval = QueueOptions {
            progress_interval: interval,
            ..options()
        };
        let error = TransferQueue::open(
            settings(1, 0),
            Runners::for_all(Arc::clone(&runner)),
            bad_interval,
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Usage, "{interval:?}");
    }
}

#[test]
fn opening_outside_a_runtime_is_an_error() {
    let runner: Arc<dyn TaskRunner> = ChunkRunner::new(1, 1, Duration::from_millis(1)).shared();
    let opening = TransferQueue::open(
        settings(1, 0),
        Runners::for_all(runner),
        QueueOptions::default(),
    );
    let error = futures::executor::block_on(opening).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Internal);
}

#[tokio::test(start_paused = true)]
async fn ten_thousand_tasks_stay_cheap() {
    const COUNT: usize = 10_000;
    let runner = ChunkRunner::new(1, 1, Duration::from_secs(3600)).shared();
    let queue = open(&runner, settings(4, 0), options()).await;
    let mut events = queue.subscribe();
    let specs: Vec<NewTask> = (0..COUNT)
        .map(|i| {
            upload(
                if i % 2 == 0 { "acct-a" } else { "acct-b" },
                &format!("t{i}"),
            )
        })
        .collect();
    let added = queue.add_tasks(specs).unwrap();
    assert_eq!(added.len(), COUNT);
    let listed = queue.list();
    assert!(listed.iter().map(|t| &t.id).eq(added.iter().map(|t| &t.id)));
    assert_eq!(
        listed
            .iter()
            .filter(|t| t.state == TransferState::Running)
            .count(),
        4
    );
    assert_eq!(drain(&mut events).len(), COUNT + 4);
    tokio::task::yield_now().await;
    assert_eq!(runner.starts(), ["t0", "t1", "t2", "t3"]);

    queue.stop_all().unwrap();
    wait_until(|| {
        queue
            .list()
            .iter()
            .all(|t| t.state == TransferState::Stopped)
    })
    .await;
    assert_eq!(drain(&mut events).len(), COUNT);

    queue.start_all().unwrap();
    assert_eq!(drain(&mut events).len(), COUNT + 4);

    queue.cancel_all().unwrap();
    wait_until(|| {
        queue
            .list()
            .iter()
            .all(|t| t.state == TransferState::Cancelled)
    })
    .await;
    assert_eq!(runner.cleanups.lock().unwrap().len(), COUNT);
    assert_eq!(drain(&mut events).len(), COUNT);

    assert_eq!(queue.clear_finished().unwrap().len(), COUNT);
    let removed = drain(&mut events);
    assert_eq!(removed.len(), 1);
    assert!(matches!(&removed[0], QueueEvent::Removed(ids) if ids.len() == COUNT));
    assert!(queue.list().is_empty());
}
