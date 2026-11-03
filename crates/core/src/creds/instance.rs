//! The role of the machine BucketBridge runs on.
//!
//! On an ECS or EKS container (the `AWS_CONTAINER_CREDENTIALS_RELATIVE_URI`
//! or `AWS_CONTAINER_CREDENTIALS_FULL_URI` variables are set) credentials
//! come from the container credentials endpoint; on an EC2 instance from the
//! instance metadata service. The metadata client asks for a session token
//! first (IMDSv2) and only falls back to IMDSv1 where the instance allows
//! it. Both providers come from the official `aws-config` crate and refresh
//! the credentials before they expire.
//!
//! [`probe`] asks for credentials once and turns "this is not an EC2
//! instance or container" into a clear error, for the account dialog and
//! `bb account add --instance-role`.

use std::time::{Duration, SystemTime};

use aws_config::ecs::EcsCredentialsProvider;
use aws_config::imds::credentials::ImdsCredentialsProvider;
use aws_config::provider_config::ProviderConfig;
use aws_credential_types::provider::{ProvideCredentials, SharedCredentialsProvider};
use aws_smithy_runtime_api::client::http::SharedHttpClient;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// How long [`probe`] waits for the metadata service in total.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// Where instance credentials come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceKind {
    /// ECS or EKS container credentials endpoint.
    Container,
    /// EC2 instance metadata service.
    Instance,
}

/// What [`probe`] found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceRoleProbe {
    /// Where the credentials came from.
    pub kind: InstanceKind,
    /// Access key id of the role session (not a secret).
    pub access_key_id: String,
    /// RFC 3339 expiry of the credentials, when the endpoint reports one.
    pub expires_at: Option<String>,
}

/// Container credentials when one of the container variables is set,
/// instance metadata otherwise.
pub fn detect_kind(get: impl Fn(&str) -> Option<String>) -> InstanceKind {
    let set = |name: &str| get(name).is_some_and(|value| !value.trim().is_empty());
    if set("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI") || set("AWS_CONTAINER_CREDENTIALS_FULL_URI") {
        InstanceKind::Container
    } else {
        InstanceKind::Instance
    }
}

/// The provider for this machine (see the module documentation).
/// `imds_endpoint` replaces the metadata service address
/// (`http://169.254.169.254`), for tests and unusual networks.
pub fn instance_provider(
    http_client: SharedHttpClient,
    imds_endpoint: Option<&str>,
) -> Result<(InstanceKind, SharedCredentialsProvider)> {
    let config = ProviderConfig::without_region().with_http_client(http_client);
    let kind = detect_kind(|name| std::env::var(name).ok());
    let provider = match kind {
        InstanceKind::Container => SharedCredentialsProvider::new(
            EcsCredentialsProvider::builder().configure(&config).build(),
        ),
        InstanceKind::Instance => {
            let mut client = aws_config::imds::Client::builder()
                .configure(&config)
                .max_attempts(2);
            if let Some(endpoint) = imds_endpoint {
                client = client.endpoint(endpoint).map_err(|error| {
                    Error::Usage(format!("invalid instance metadata endpoint: {error}"))
                })?;
            }
            SharedCredentialsProvider::new(
                ImdsCredentialsProvider::builder()
                    .configure(&config)
                    .imds_client(client.build())
                    .build(),
            )
        }
    };
    Ok((kind, provider))
}

/// Fetches credentials once to prove the machine has a role. Fails with an
/// `auth` error that says the machine is not an EC2 instance or container
/// (or has no role attached) when nothing answers.
pub async fn probe(
    http_client: SharedHttpClient,
    imds_endpoint: Option<&str>,
) -> Result<InstanceRoleProbe> {
    let (kind, provider) = instance_provider(http_client, imds_endpoint)?;
    probe_provider(kind, &provider).await
}

pub(crate) async fn probe_provider(
    kind: InstanceKind,
    provider: &SharedCredentialsProvider,
) -> Result<InstanceRoleProbe> {
    let outcome = tokio::time::timeout(PROBE_TIMEOUT, provider.provide_credentials()).await;
    match outcome {
        Ok(Ok(credentials)) => Ok(InstanceRoleProbe {
            kind,
            access_key_id: credentials.access_key_id().to_owned(),
            expires_at: credentials.expiry().map(rfc3339),
        }),
        Ok(Err(error)) => Err(not_available(kind, &super::error_chain(&error))),
        Err(_) => Err(not_available(kind, "no answer in time")),
    }
}

fn not_available(kind: InstanceKind, detail: &str) -> Error {
    let place = match kind {
        InstanceKind::Container => {
            "the container credentials endpoint (AWS_CONTAINER_CREDENTIALS_RELATIVE_URI or \
             AWS_CONTAINER_CREDENTIALS_FULL_URI) did not return credentials"
        }
        InstanceKind::Instance => {
            "the EC2 instance metadata service did not return credentials: this is not an EC2 \
             instance, no role is attached to it, or the service is blocked"
        }
    };
    Error::Auth(format!("no instance or container role: {place} ({detail})"))
}

fn rfc3339(time: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
pub(crate) mod testing {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A minimal blocking HTTP server on a thread: answers each request with
    /// `respond(method, path)` as (status, body) and counts the requests.
    pub(crate) struct Fake {
        pub(crate) url: String,
        pub(crate) hits: Arc<AtomicUsize>,
        pub(crate) paths: Arc<std::sync::Mutex<Vec<String>>>,
    }

    pub(crate) fn serve(respond: impl Fn(&str, &str) -> (u16, String) + Send + 'static) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let paths = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (hits_in, paths_in) = (Arc::clone(&hits), Arc::clone(&paths));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buffer = [0u8; 4096];
                let Ok(read) = stream.read(&mut buffer) else {
                    continue;
                };
                let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                let mut first = request.lines().next().unwrap_or("").split(' ');
                let method = first.next().unwrap_or("").to_owned();
                let path = first.next().unwrap_or("").to_owned();
                hits_in.fetch_add(1, Ordering::SeqCst);
                paths_in.lock().unwrap().push(format!("{method} {path}"));
                let (status, body) = respond(&method, &path);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\nx-aws-ec2-metadata-token-ttl-seconds: 21600\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        Fake { url, hits, paths }
    }

    pub(crate) fn imds(role: Option<&'static str>) -> Fake {
        serve(move |method, path| {
            match (method, path) {
            ("PUT", "/latest/api/token") => (200, "TOKEN".into()),
            ("GET", "/latest/meta-data/iam/security-credentials/") => match role {
                Some(role) => (200, role.into()),
                None => (404, String::new()),
            },
            ("GET", p) if p.starts_with("/latest/meta-data/iam/security-credentials/") => (
                200,
                r#"{"Code":"Success","Type":"AWS-HMAC","AccessKeyId":"ASIAINSTANCE","SecretAccessKey":"instance-secret","Token":"instance-token","Expiration":"2099-01-01T00:00:00Z"}"#.into(),
            ),
            _ => (404, String::new()),
        }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::sync::atomic::Ordering;

    use super::testing::{imds, serve};
    use super::*;

    fn http() -> SharedHttpClient {
        aws_smithy_http_client::Builder::new().build_http()
    }

    #[test]
    fn container_variables_select_container_credentials() {
        assert_eq!(detect_kind(|_| None), InstanceKind::Instance);
        let relative = |name: &str| {
            (name == "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI").then(|| "/creds".to_owned())
        };
        assert_eq!(detect_kind(relative), InstanceKind::Container);
        let full = |name: &str| {
            (name == "AWS_CONTAINER_CREDENTIALS_FULL_URI")
                .then(|| "http://127.0.0.1/creds".to_owned())
        };
        assert_eq!(detect_kind(full), InstanceKind::Container);
        assert_eq!(detect_kind(|_| Some(" ".into())), InstanceKind::Instance);
    }

    #[tokio::test]
    async fn imdsv2_credentials_are_fetched_with_a_session_token() {
        let fake = imds(Some("my-role"));
        let probe = probe(http(), Some(&fake.url)).await.unwrap();
        assert_eq!(probe.kind, InstanceKind::Instance);
        assert_eq!(probe.access_key_id, "ASIAINSTANCE");
        assert_eq!(probe.expires_at.as_deref(), Some("2099-01-01T00:00:00Z"));
        let paths = fake.paths.lock().unwrap().clone();
        assert_eq!(paths[0], "PUT /latest/api/token");
        assert!(
            paths
                .iter()
                .any(|p| p == "GET /latest/meta-data/iam/security-credentials/my-role")
        );
    }

    #[tokio::test]
    async fn the_probe_does_not_leak_the_secret() {
        let fake = imds(Some("my-role"));
        let found = probe(http(), Some(&fake.url)).await.unwrap();
        let text = format!("{found:?}{}", serde_json::to_string(&found).unwrap());
        assert!(!text.contains("instance-secret") && !text.contains("instance-token"));
    }

    #[tokio::test]
    async fn no_attached_role_is_a_clear_error() {
        let fake = imds(None);
        let error = probe(http(), Some(&fake.url)).await.unwrap_err();
        assert_eq!(error.kind(), crate::ErrorKind::Auth);
        let text = error.to_string();
        assert!(text.contains("not an EC2 instance"), "{text}");
        assert!(fake.hits.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn an_unreachable_service_is_a_clear_error() {
        // A port nothing listens on.
        let free = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", free.local_addr().unwrap());
        drop(free);
        let error = probe(http(), Some(&url)).await.unwrap_err();
        assert_eq!(error.kind(), crate::ErrorKind::Auth);
        assert!(error.to_string().contains("instance metadata"), "{error}");
    }

    #[tokio::test]
    async fn a_bad_endpoint_is_a_usage_error() {
        let error = probe(http(), Some("not a url")).await.unwrap_err();
        assert_eq!(error.kind(), crate::ErrorKind::Usage);
    }

    /// Runs the ECS provider in a child process, which has the container
    /// variables (the test process itself must not set variables).
    #[tokio::test]
    async fn container_credentials_come_from_the_full_uri() {
        if std::env::var("BB_TEST_ECS_CHILD").is_ok() {
            return;
        }
        let fake = serve(|_, path| {
            if path == "/creds" {
                (
                    200,
                    r#"{"AccessKeyId":"ASIACONTAINER","SecretAccessKey":"container-secret","Token":"container-token","Expiration":"2099-01-01T00:00:00Z"}"#.into(),
                )
            } else {
                (404, String::new())
            }
        });
        let exe = std::env::current_exe().unwrap();
        let url = format!("{}/creds", fake.url);
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new(exe)
                .args([
                    "--exact",
                    "creds::instance::tests::container_child",
                    "--nocapture",
                    "--ignored",
                ])
                .env("BB_TEST_ECS_CHILD", "1")
                .env("AWS_CONTAINER_CREDENTIALS_FULL_URI", url)
                .output()
        })
        .await
        .unwrap()
        .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}");
        assert!(
            stdout.contains("ECS_OK ASIACONTAINER Container"),
            "{stdout}"
        );
    }

    #[tokio::test]
    #[ignore = "run by container_credentials_come_from_the_full_uri in a child process"]
    async fn container_child() {
        if std::env::var("BB_TEST_ECS_CHILD").is_err() {
            return;
        }
        let found = probe(http(), None).await.unwrap();
        println!("ECS_OK {} {:?}", found.access_key_id, found.kind);
    }
}
