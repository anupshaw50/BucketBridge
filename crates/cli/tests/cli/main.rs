//! Runs `bb` against isolated test servers: a `TestS3` (s3s-fs, or MinIO
//! with `--features minio` and `BB_TEST_BACKEND=minio`) or a scripted
//! `MockS3` for behaviour s3s-fs lacks. Most tests run the real binary;
//! account tests run the library in-process with an in-memory secret
//! store, so they never touch the OS keychain. One test binary, one module
//! per command.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod account;
mod buckets;
mod cloudfront;
mod copy;
mod credentials;
mod cse;
mod edit;
mod export;
mod global;
mod iam;
mod local;
mod ls;
mod mpu;
mod objectlock;
mod objectmeta;
mod paging;
mod presign;
mod rm;
mod scan;
mod sync;
mod syncjobs;
mod transfers;
mod versions;

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use assert_cmd::cargo::cargo_bin;
use bucketbridge_cli::{Console, Io, Secrets};
use bucketbridge_core::Dirs;
use bucketbridge_core::secrets::{KdfCost, MemoryStore};
use bucketbridge_core::session::{Session, SessionOptions};
use bucketbridge_tests::{Backend, MockS3, TestS3};
use tempfile::TempDir;

// Assertions go through `bucketbridge_cli::i18n::tr`, so each test holds
// under either interface language; no environment pinning is needed.

/// Every variable `bb` reads, removed from the inherited environment.
pub const BB_VARS: [&str; 8] = [
    "BB_ACCESS_KEY_ID",
    "BB_SECRET_ACCESS_KEY",
    "BB_SESSION_TOKEN",
    "BB_REGION",
    "BB_ENDPOINT",
    "BB_PATH_STYLE",
    "BB_CONFIG_DIR",
    "BB_MASTER_PASSWORD",
];

/// Proxy variables honoured by the HTTP client, removed as well.
pub const PROXY_VARS: [&str; 6] = [
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
];

/// The `bb` run produced.
pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// The batch summary line ("2 of 3 objects could not be deleted"),
/// translated exactly like the code builds it.
pub fn batch_summary(failed: usize, total: usize, noun: &str, failure: &str) -> String {
    use bucketbridge_cli::i18n::{tr, trf};
    trf(
        "{failed} of {total} {noun} {failure}",
        &[
            ("failed", failed.to_string().as_str()),
            ("total", total.to_string().as_str()),
            ("noun", tr(noun).as_str()),
            ("failure", tr(failure).as_str()),
        ],
    )
}

impl Output {
    pub fn lines(&self) -> Vec<&str> {
        self.stdout.lines().collect()
    }

    /// The NDJSON records on stdout.
    pub fn records(&self) -> Vec<serde_json::Value> {
        self.stdout
            .lines()
            .map(|line| {
                serde_json::from_str(line).unwrap_or_else(|err| panic!("not JSON: {line}: {err}"))
            })
            .collect()
    }

    /// The single JSON error record on stderr (`--json`).
    pub fn error_record(&self) -> serde_json::Value {
        let lines: Vec<&str> = self.stderr.lines().collect();
        assert_eq!(lines.len(), 1, "expected one stderr line: {}", self.stderr);
        serde_json::from_str(lines[0]).unwrap_or_else(|err| panic!("not JSON: {}: {err}", lines[0]))
    }

    /// Asserts the exit code, printing stderr on mismatch.
    pub fn expect_code(&self, code: i32) -> &Self {
        assert_eq!(
            self.code, code,
            "stdout: {}\nstderr: {}",
            self.stdout, self.stderr
        );
        self
    }
}

/// Runs a prepared command.
pub fn run(cmd: &mut assert_cmd::Command) -> Output {
    let output = cmd.output().expect("run bb");
    Output {
        code: output.status.code().expect("bb exited normally"),
        stdout: String::from_utf8(output.stdout).expect("UTF-8 stdout"),
        stderr: String::from_utf8(output.stderr).expect("UTF-8 stderr"),
    }
}

/// The `bb` binary as a plain process with a hermetic environment: no
/// inherited `BB_*` or proxy variables and a private config directory.
pub fn bb_process(config_dir: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(cargo_bin!("bb"));
    for var in BB_VARS.iter().chain(PROXY_VARS.iter()) {
        cmd.env_remove(var);
    }
    cmd.env("BB_CONFIG_DIR", config_dir);
    cmd
}

/// `bb` with a hermetic environment (see [`bb_process`]).
pub fn bb(config_dir: &Path) -> assert_cmd::Command {
    assert_cmd::Command::from_std(bb_process(config_dir))
}

/// Sets the variables that point `bb` at a scripted endpoint (it accepts
/// any credentials).
pub fn mock_env(cmd: &mut std::process::Command, mock: &MockS3) {
    cmd.env("BB_ACCESS_KEY_ID", "AKIDMOCK")
        .env("BB_SECRET_ACCESS_KEY", "mock-secret")
        .env("BB_ENDPOINT", mock.endpoint())
        .env("BB_PATH_STYLE", "true");
}

/// `bb` pointed at a scripted endpoint.
pub fn bb_mock(mock: &MockS3, config_dir: &Path) -> assert_cmd::Command {
    let mut cmd = bb_process(config_dir);
    mock_env(&mut cmd, mock);
    assert_cmd::Command::from_std(cmd)
}

/// A test server and a private config directory.
pub struct Env {
    pub s3: TestS3,
    pub config: TempDir,
}

impl Env {
    pub async fn start() -> Env {
        Env {
            s3: TestS3::start().await,
            config: tempfile::tempdir().unwrap(),
        }
    }

    pub async fn start_minio() -> Env {
        Env {
            s3: TestS3::start_minio().await,
            config: tempfile::tempdir().unwrap(),
        }
    }

    /// The `bb` process with valid credentials for the server.
    pub fn process(&self) -> std::process::Command {
        let mut cmd = bb_process(self.config.path());
        cmd.env("BB_ACCESS_KEY_ID", self.s3.access_key())
            .env("BB_SECRET_ACCESS_KEY", self.s3.secret_key())
            .env("BB_REGION", self.s3.region())
            .env("BB_ENDPOINT", self.s3.endpoint())
            .env("BB_PATH_STYLE", "true");
        cmd
    }

    /// `bb` with valid credentials for the server.
    pub fn bb(&self) -> assert_cmd::Command {
        assert_cmd::Command::from_std(self.process())
    }

    /// Runs `bb` with `args` against the server.
    pub fn run(&self, args: &[&str]) -> Output {
        run(self.bb().args(args))
    }

    /// Runs `bb` with `args` in the working directory `dir`.
    pub fn run_in(&self, dir: &Path, args: &[&str]) -> Output {
        run(self.bb().current_dir(dir).args(args))
    }

    /// Whether `key` exists as a folder marker (s3s-fs stores markers as
    /// directories and never lists them; see docs/testing.md).
    pub async fn marker_exists(&self, bucket: &str, key: &str) -> bool {
        match self.s3.backend() {
            Backend::S3sFs => self
                .s3
                .root()
                .unwrap()
                .join(bucket)
                .join(key.trim_end_matches('/'))
                .is_dir(),
            Backend::MinIO => self.keys(bucket).await.iter().any(|k| k == key),
        }
    }

    /// Keys of a bucket (recursive), via the core transport.
    pub async fn keys(&self, bucket: &str) -> Vec<String> {
        self.s3
            .ops()
            .list_all(bucket, "")
            .await
            .unwrap()
            .into_iter()
            .map(|entry| entry.key)
            .collect()
    }
}

/// A writer that keeps everything written to it.
#[derive(Clone, Default)]
pub struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    pub fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).expect("UTF-8 output")
    }
}

impl Write for Captured {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A terminal that answers secret prompts from a list (no answer: there is
/// no terminal) and remembers the prompts.
#[derive(Clone, Default)]
pub struct Terminal {
    answers: Arc<Mutex<VecDeque<String>>>,
    prompts: Arc<Mutex<Vec<String>>>,
}

impl Terminal {
    pub fn answering(answers: &[&str]) -> Terminal {
        Terminal {
            answers: Arc::new(Mutex::new(
                answers.iter().map(|a| (*a).to_owned()).collect(),
            )),
            prompts: Arc::default(),
        }
    }

    pub fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }
}

impl Console for Terminal {
    fn read_secret(&self, prompt: &str) -> io::Result<Option<String>> {
        self.prompts.lock().unwrap().push(prompt.to_owned());
        Ok(self.answers.lock().unwrap().pop_front())
    }

    fn stderr_is_terminal(&self) -> bool {
        false
    }
}

/// `bb` run in-process with a private config directory and an in-memory
/// secret store (never the OS keychain).
pub struct InProcess {
    pub config: TempDir,
    pub store: Arc<MemoryStore>,
    /// Customizes the IAM client (a mock, see `iam.rs`).
    pub iam_hook: Option<bucketbridge_core::session::IamConfigHook>,
    /// Adjusts the session options (credential sources, see `credentials.rs`).
    pub options_hook: Option<bucketbridge_cli::SessionOptionsHook>,
}

impl Default for InProcess {
    fn default() -> InProcess {
        InProcess {
            config: tempfile::tempdir().unwrap(),
            store: Arc::new(MemoryStore::new()),
            iam_hook: None,
            options_hook: None,
        }
    }
}

impl InProcess {
    /// A cheap key derivation for the master passwords of tests.
    pub const KDF: KdfCost = KdfCost {
        m_cost_kib: 1024,
        t_cost: 1,
        p_cost: 1,
    };

    /// Runs `bb args` with nothing on stdin and no terminal.
    pub async fn run(&self, args: &[&str]) -> Output {
        self.run_with(args, &[], b"", &Terminal::default()).await
    }

    /// Runs `bb args` with only `BB_CONFIG_DIR` and `vars` set, `stdin` on
    /// standard input and `terminal` answering prompts.
    pub async fn run_with(
        &self,
        args: &[&str],
        vars: &[(&str, &str)],
        stdin: &[u8],
        terminal: &Terminal,
    ) -> Output {
        let stdout = Captured::default();
        let stderr = Captured::default();
        let mut env: Vec<(String, OsString)> = vec![(
            "BB_CONFIG_DIR".to_owned(),
            self.config.path().as_os_str().to_owned(),
        )];
        env.extend(
            vars.iter()
                .map(|(name, value)| ((*name).to_owned(), OsString::from(value))),
        );
        let io = Io {
            env: bucketbridge_cli::Env::from_pairs(env),
            stdin: Box::new(io::Cursor::new(stdin.to_vec())),
            stdout: Box::new(stdout.clone()),
            stderr: Box::new(stderr.clone()),
            console: Box::new(terminal.clone()),
        };
        let secrets = Secrets::Store {
            store: self.store.clone(),
            kdf_cost: Self::KDF,
            iam_config_hook: self.iam_hook.clone(),
            cloudfront_config_hook: None,
            options_hook: self.options_hook.clone(),
        };
        let args = std::iter::once("bb").chain(args.iter().copied());
        let exit = bucketbridge_cli::run(args, io, secrets, std::future::pending::<()>).await;
        Output {
            code: i32::from(exit.code),
            stdout: stdout.text(),
            stderr: stderr.text(),
        }
    }

    /// The session the commands use, for setting things up directly.
    pub fn session(&self) -> Session {
        let dirs = Dirs::from_override(Some(self.config.path().as_os_str().to_owned())).unwrap();
        Session::open_with(
            dirs,
            self.store.clone(),
            SessionOptions {
                kdf_cost: Self::KDF,
                ..SessionOptions::default()
            },
        )
        .unwrap()
    }

    /// The text of config.toml.
    pub fn config_text(&self) -> String {
        std::fs::read_to_string(self.config.path().join("config.toml")).unwrap_or_default()
    }
}

/// XML documents for `MockS3` replies.
pub mod xml {
    pub const NS: &str = "xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"";

    /// A `ListObjectVersions` reply.
    pub fn versions(
        entries: &[(&str, &str, bool, bool)],
        prefixes: &[&str],
        next: Option<(&str, &str)>,
    ) -> String {
        let mut body = String::new();
        for (key, version_id, is_latest, is_marker) in entries {
            if *is_marker {
                body.push_str(&format!(
                    "<DeleteMarker><Key>{key}</Key><VersionId>{version_id}</VersionId><IsLatest>{is_latest}</IsLatest><LastModified>2026-10-29T08:15:00.000Z</LastModified></DeleteMarker>"
                ));
            } else {
                body.push_str(&format!(
                    "<Version><Key>{key}</Key><VersionId>{version_id}</VersionId><IsLatest>{is_latest}</IsLatest><LastModified>2026-10-29T08:15:00.000Z</LastModified><ETag>\"etag\"</ETag><Size>5</Size><StorageClass>STANDARD</StorageClass></Version>"
                ));
            }
        }
        for prefix in prefixes {
            body.push_str(&format!(
                "<CommonPrefixes><Prefix>{prefix}</Prefix></CommonPrefixes>"
            ));
        }
        let truncation = match next {
            Some((key, version)) => format!(
                "<IsTruncated>true</IsTruncated><NextKeyMarker>{key}</NextKeyMarker><NextVersionIdMarker>{version}</NextVersionIdMarker>"
            ),
            None => "<IsTruncated>false</IsTruncated>".to_owned(),
        };
        format!(
            "<ListVersionsResult {NS}><Name>bucket</Name>{truncation}{body}</ListVersionsResult>"
        )
    }

    /// A `ListMultipartUploads` reply.
    pub fn uploads(bucket: &str, uploads: &[(&str, &str)]) -> String {
        let body: String = uploads
            .iter()
            .map(|(key, id)| {
                format!(
                    "<Upload><Key>{key}</Key><UploadId>{id}</UploadId><StorageClass>STANDARD</StorageClass><Initiated>2026-10-29T08:15:00.000Z</Initiated></Upload>"
                )
            })
            .collect();
        format!(
            "<ListMultipartUploadsResult {NS}><Bucket>{bucket}</Bucket><IsTruncated>false</IsTruncated>{body}</ListMultipartUploadsResult>"
        )
    }

    /// A `ListBuckets` reply.
    pub fn buckets(buckets: &[(&str, &str)]) -> String {
        let body: String = buckets
            .iter()
            .map(|(name, region)| {
                format!(
                    "<Bucket><Name>{name}</Name><CreationDate>2026-10-29T08:15:00.000Z</CreationDate><BucketRegion>{region}</BucketRegion></Bucket>"
                )
            })
            .collect();
        format!(
            "<ListAllMyBucketsResult {NS}><Owner><ID>owner-id</ID></Owner><Buckets>{body}</Buckets></ListAllMyBucketsResult>"
        )
    }

    /// A `ListObjectsV2` reply with the given keys (the last page).
    pub fn objects(keys: &[&str]) -> String {
        objects_page(keys, None)
    }

    /// A `ListObjectsV2` reply with the given keys and, when `next` is set,
    /// more results behind that continuation token.
    pub fn objects_page(keys: &[&str], next: Option<&str>) -> String {
        let body: String = keys
            .iter()
            .map(|key| format!("<Contents><Key>{key}</Key><Size>1</Size></Contents>"))
            .collect();
        let truncation = match next {
            Some(token) => format!(
                "<IsTruncated>true</IsTruncated><NextContinuationToken>{token}</NextContinuationToken>"
            ),
            None => "<IsTruncated>false</IsTruncated>".to_owned(),
        };
        format!(
            "<ListBucketResult {NS}><Name>bucket</Name><KeyCount>{}</KeyCount>{truncation}{body}</ListBucketResult>",
            keys.len()
        )
    }
}
