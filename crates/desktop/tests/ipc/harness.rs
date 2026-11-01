//! A mock-runtime app with the real command handler and state: commands are
//! invoked through Tauri's IPC path with JSON arguments, exactly as the UI
//! sends them, and events are received with Rust listeners.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bucketbridge_core::secrets::{KdfCost, MemoryStore};
use bucketbridge_core::session::SessionOptions;
use bucketbridge_core::{Dirs, EventLog, Session};
use bucketbridge_desktop::{
    AppState, CommandError, Core, CoreParts, OpenWith, Opener, UrlOpener, forward_events,
};
use bucketbridge_tests::TestS3;
use serde_json::{Value, json};
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{
    INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder, mock_context, noop_assets,
};
use tauri::webview::InvokeRequest;
use tauri::{App, Listener, Manager, WebviewWindow, WebviewWindowBuilder};

/// How long a test waits for an event.
pub const EVENT_TIMEOUT: Duration = Duration::from_secs(60);

/// How often a test polls the queue.
const POLL: Duration = Duration::from_millis(10);

/// One app per test. Fields drop in order: the app, then the core (which
/// saves the transfer queue), then the folder it uses.
pub struct Harness {
    webview: WebviewWindow<MockRuntime>,
    app: App<MockRuntime>,
    pub core: Arc<Core>,
    _stop: StopCore,
    /// Paths handed to the opener by `open_object`.
    pub opened: Arc<Mutex<Vec<PathBuf>>>,
    /// File and application handed to the opener by `open_object_with`.
    pub opened_with: Arc<Mutex<Vec<(PathBuf, String)>>>,
    /// URLs handed to the browser opener by `open_bucket_in_browser`.
    pub urls: Arc<Mutex<Vec<String>>>,
    /// The secrets of the accounts; a restarted app keeps them.
    pub secrets: Arc<MemoryStore>,
    pub dir: tempfile::TempDir,
}

/// Stops the core when the harness goes away: the transfer queue is saved
/// while the application folder still exists.
struct StopCore(Arc<Core>);

impl Drop for StopCore {
    fn drop(&mut self) {
        self.0.shutdown(Duration::from_secs(30));
    }
}

/// What the recording openers were asked to open.
#[derive(Clone, Default)]
pub struct Recorded {
    pub opened: Arc<Mutex<Vec<PathBuf>>>,
    pub opened_with: Arc<Mutex<Vec<(PathBuf, String)>>>,
    pub urls: Arc<Mutex<Vec<String>>>,
}

/// The parts of a core over `<dir>/app`, with the opener recording what it
/// is asked to open.
pub fn core_parts(dir: &Path, secrets: Arc<MemoryStore>) -> (CoreParts, Recorded) {
    core_parts_with(dir, secrets, |options| options)
}

/// [`core_parts`] with `configure` applied to the session options.
pub fn core_parts_with(
    dir: &Path,
    secrets: Arc<MemoryStore>,
    configure: impl FnOnce(SessionOptions) -> SessionOptions,
) -> (CoreParts, Recorded) {
    let dirs = Dirs::under(dir.join("app"));
    dirs.create_all().unwrap();
    let options = SessionOptions {
        // The cheapest Argon2id cost: these tests are not about the KDF.
        kdf_cost: KdfCost {
            m_cost_kib: 8,
            t_cost: 1,
            p_cost: 1,
        },
        // Proxy variables of the machine running the tests must not apply.
        environment_proxy: false,
        ..SessionOptions::default()
    };
    let options = configure(options);
    let session = Session::open_with(dirs.clone(), secrets, options).unwrap();
    let log = EventLog::new(&Default::default(), None);
    let recorded = Recorded::default();
    let recorder = Arc::clone(&recorded.opened);
    let opener: Opener = Arc::new(move |path: &Path| {
        recorder.lock().unwrap().push(path.to_path_buf());
        Ok(())
    });
    let recorder = Arc::clone(&recorded.opened_with);
    let open_with: OpenWith = Arc::new(move |path: &Path, app: &str| {
        if app.contains("broken") {
            return Err("cannot start the application".to_owned());
        }
        recorder
            .lock()
            .unwrap()
            .push((path.to_path_buf(), app.to_owned()));
        Ok(())
    });
    let recorder = Arc::clone(&recorded.urls);
    let url_opener: UrlOpener = Arc::new(move |url: &str| {
        recorder.lock().unwrap().push(url.to_owned());
        Ok(())
    });
    (
        CoreParts {
            dirs,
            session,
            log,
            opener,
            open_with,
            url_opener,
        },
        recorded,
    )
}

impl Harness {
    /// An app over a fresh application folder, an in-memory secret store and
    /// an event log without a log file.
    pub fn new() -> Harness {
        Self::open(tempfile::tempdir().unwrap(), Arc::new(MemoryStore::new()))
    }

    /// Like [`Harness::new`], with `configure` applied to the session options
    /// (for example to inject a CloudFront test client).
    pub fn with_options(configure: impl FnOnce(&mut SessionOptions)) -> Harness {
        Self::with_session_options(|mut options| {
            configure(&mut options);
            options
        })
    }

    /// A fresh app whose session options are changed by `configure`.
    pub fn with_session_options(
        configure: impl FnOnce(SessionOptions) -> SessionOptions,
    ) -> Harness {
        Self::open_with(
            tempfile::tempdir().unwrap(),
            Arc::new(MemoryStore::new()),
            configure,
        )
    }

    fn open(dir: tempfile::TempDir, secrets: Arc<MemoryStore>) -> Harness {
        Self::open_with(dir, secrets, |options| options)
    }

    fn open_with(
        dir: tempfile::TempDir,
        secrets: Arc<MemoryStore>,
        configure: impl FnOnce(SessionOptions) -> SessionOptions,
    ) -> Harness {
        let (parts, recorded) = core_parts_with(dir.path(), Arc::clone(&secrets), configure);
        let core = Core::new(parts).unwrap();
        let app = mock_app(AppState::ready(Arc::clone(&core)));
        forward_events(app.handle(), &core);
        let webview = webview(&app);
        Harness {
            webview,
            app,
            _stop: StopCore(Arc::clone(&core)),
            core,
            opened: recorded.opened,
            opened_with: recorded.opened_with,
            urls: recorded.urls,
            secrets,
            dir,
        }
    }

    /// Quits the app (running transfers pause and the queue is saved) and
    /// starts a new one over the same folder and secrets, as a restart of
    /// the program does.
    pub fn restart(self) -> Harness {
        let Harness {
            webview,
            app,
            core,
            _stop: stop,
            secrets,
            dir,
            ..
        } = self;
        drop(webview);
        drop(app);
        core.shutdown(Duration::from_secs(30));
        drop(stop);
        drop(core);
        Self::open(dir, secrets)
    }

    /// Invokes `command` with `args` through IPC: `Ok(result)` or
    /// `Err(rejection)`, both as JSON.
    pub fn invoke(&self, command: &str, args: Value) -> Result<Value, Value> {
        invoke(&self.webview, command, args)
    }

    /// Invokes a command that must succeed.
    pub fn ok(&self, command: &str, args: Value) -> Value {
        self.invoke(command, args)
            .unwrap_or_else(|err| panic!("{command} failed: {err}"))
    }

    /// Invokes a command that must fail with a `BbError`; returns its code.
    pub fn code(&self, command: &str, args: Value) -> String {
        self.error(command, args).0
    }

    /// Invokes a command that must fail with a `BbError`; returns its code and
    /// message.
    pub fn error(&self, command: &str, args: Value) -> (String, String) {
        bb_error(command, self.invoke(command, args))
    }

    /// Labels of the windows the app has open, sorted.
    pub fn window_labels(&self) -> Vec<String> {
        let mut labels: Vec<String> = self.app.webview_windows().into_keys().collect();
        labels.sort();
        labels
    }

    /// Receives the payloads of `event` from now on.
    pub fn events(&self, event: &str) -> Receiver<Value> {
        let (tx, rx) = mpsc::channel();
        self.app.listen(event.to_owned(), move |event| {
            let payload: Value = serde_json::from_str(event.payload()).unwrap();
            let _ = tx.send(payload);
        });
        rx
    }

    /// Adds an account for `server` and returns its id.
    pub fn add_account(&self, server: &TestS3, label: &str) -> String {
        let account = self.ok(
            "add_account",
            json!({ "input": account_input(server, label) }),
        );
        account["id"].as_str().unwrap().to_owned()
    }

    /// Changes one settings value through `get_settings` and `save_settings`.
    pub fn set_setting(&self, section: &str, field: &str, value: Value) {
        let mut settings = self.ok("get_settings", json!({}));
        settings[section][field] = value;
        self.ok("save_settings", json!({ "settings": settings }));
    }

    /// Every task, from `list_tasks`.
    pub fn tasks(&self) -> Vec<Value> {
        self.ok("list_tasks", json!({})).as_array().unwrap().clone()
    }

    /// The task `id`, from `list_tasks`.
    pub fn task(&self, id: &str) -> Value {
        self.tasks()
            .into_iter()
            .find(|task| task["id"] == id)
            .unwrap_or_else(|| panic!("task {id} is not in the queue"))
    }

    /// Waits until task `id` is in `state` and returns it. Fails at once when
    /// the task ends in another final state, with the task as the message.
    /// (A `failed` task is not final while it is being cancelled: it keeps
    /// that state until the cleanup is done.)
    pub fn wait_state(&self, id: &str, state: &str) -> Value {
        let deadline = Instant::now() + EVENT_TIMEOUT;
        loop {
            let task = self.task(id);
            let now = task["state"].as_str().unwrap();
            if now == state {
                return task;
            }
            let ended = match now {
                "completed" | "cancelled" => true,
                "failed" => state != "cancelled",
                _ => false,
            };
            assert!(!ended, "task {id} ended as {now}, not {state}: {task}");
            assert!(
                Instant::now() < deadline,
                "task {id} is still {now}, not {state}: {task}"
            );
            std::thread::sleep(POLL);
        }
    }

    /// Waits until `check` holds for task `id`; returns that task.
    pub fn wait_task(&self, id: &str, what: &str, check: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + EVENT_TIMEOUT;
        loop {
            let task = self.task(id);
            if check(&task) {
                return task;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for task {id}: {what}: {task}"
            );
            std::thread::sleep(POLL);
        }
    }
}

/// A mock app with the command handler and `state`.
pub fn mock_app(state: AppState) -> App<MockRuntime> {
    mock_builder()
        .invoke_handler(bucketbridge_desktop::handler())
        .manage(state)
        .build(mock_context(noop_assets()))
        .unwrap()
}

/// A webview of `app` to invoke commands from.
pub fn webview(app: &App<MockRuntime>) -> WebviewWindow<MockRuntime> {
    WebviewWindowBuilder::new(app, "main", Default::default())
        .build()
        .unwrap()
}

/// Invokes `command` from `webview` the way `invoke()` of `@tauri-apps/api`
/// does: a JSON body of `snake_case` arguments.
pub fn invoke(
    webview: &WebviewWindow<MockRuntime>,
    command: &str,
    args: Value,
) -> Result<Value, Value> {
    let url = if cfg!(windows) {
        "http://tauri.localhost"
    } else {
        "tauri://localhost"
    };
    get_ipc_response(
        webview,
        InvokeRequest {
            cmd: command.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: url.parse().unwrap(),
            body: InvokeBody::Json(args),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    )
    .map(|body| body.deserialize::<Value>().unwrap())
}

/// The code and message of a rejection, which must be a `BbError` object
/// with exactly the keys `code` and `message`.
pub fn bb_error(command: &str, result: Result<Value, Value>) -> (String, String) {
    let error = match result {
        Ok(value) => panic!("{command} succeeded with {value} but should have failed"),
        Err(error) => error,
    };
    let object = error
        .as_object()
        .unwrap_or_else(|| panic!("{command} rejected with {error}, not a BbError object"));
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["code", "message"], "{command}: {error}");
    let message = object["message"].as_str().unwrap().to_owned();
    assert!(!message.is_empty(), "{command}: empty message");
    (object["code"].as_str().unwrap().to_owned(), message)
}

/// The sorted keys of a JSON object.
pub fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .unwrap_or_else(|| panic!("{value} is not an object"))
        .keys()
        .cloned()
        .collect();
    keys.sort_unstable();
    keys
}

/// The `AccountInput` of an account on `server`.
pub fn account_input(server: &TestS3, label: &str) -> Value {
    json!({
        "label": label,
        "access_key_id": server.access_key(),
        "secret_access_key": server.secret_key(),
        "session_token": null,
        "region": server.region(),
        "endpoint": server.endpoint(),
        "path_style": true,
        "request_payer": false,
        "mfa_serial": null,
    })
}

/// An `AccountUpdate` that changes nothing.
pub fn no_change() -> Value {
    json!({
        "label": null,
        "access_key_id": null,
        "secret_access_key": null,
        "session_token": null,
        "clear_session_token": false,
        "region": null,
        "endpoint": null,
        "clear_endpoint": false,
        "path_style": null,
        "request_payer": null,
        "mfa_serial": null,
        "clear_mfa_serial": false,
    })
}

/// A fresh in-process S3 server.
pub fn start_s3() -> TestS3 {
    block_on(TestS3::start())
}

/// Runs a future on Tauri's async runtime.
pub fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tauri::async_runtime::block_on(future)
}

/// The next payload, waiting at most [`EVENT_TIMEOUT`].
pub fn next(events: &Receiver<Value>) -> Value {
    events
        .recv_timeout(EVENT_TIMEOUT)
        .expect("the event did not arrive")
}

/// Receives payloads until one has `finished: true`, and returns it.
pub fn until_finished(events: &Receiver<Value>) -> Value {
    loop {
        let progress = next(events);
        if progress["finished"] == json!(true) {
            return progress;
        }
    }
}

/// Receives `transfer://update` payloads until every task in `ids` has
/// reported `state`; returns the first such payload of each task. Events of
/// other tasks, and earlier states, are skipped.
pub fn until_all(events: &Receiver<Value>, ids: &[String], state: &str) -> HashMap<String, Value> {
    let mut seen = HashMap::new();
    while seen.len() < ids.len() {
        let task = next(events);
        let id = task["id"].as_str().unwrap().to_owned();
        if task["state"] == state && ids.contains(&id) {
            seen.entry(id).or_insert(task);
        }
    }
    seen
}

/// Receives `transfer://update` payloads until every `(task id, state)` pair
/// of `wanted` has been seen, in any order; other events are skipped.
pub fn expect_updates(events: &Receiver<Value>, wanted: &[(&str, &str)]) {
    let mut missing = wanted.to_vec();
    while !missing.is_empty() {
        let task = next(events);
        if let Some(at) = missing
            .iter()
            .position(|(id, state)| task["id"] == *id && task["state"] == *state)
        {
            missing.remove(at);
        }
    }
}

/// The ids of a `transfer://removed` payload, sorted.
pub fn removed_ids(event: &Value) -> Vec<String> {
    assert_eq!(keys(event), ["ids"], "{event}");
    let mut ids: Vec<String> = event["ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().to_owned())
        .collect();
    ids.sort_unstable();
    ids
}

/// The ids of the tasks in a command's result.
pub fn ids(tasks: &Value) -> Vec<String> {
    tasks
        .as_array()
        .unwrap()
        .iter()
        .map(|task| task["id"].as_str().unwrap().to_owned())
        .collect()
}

/// The error a failed startup leaves for every command.
pub fn startup_error(message: &str) -> CommandError {
    CommandError::new(bucketbridge_core::ErrorKind::Io, message)
}
