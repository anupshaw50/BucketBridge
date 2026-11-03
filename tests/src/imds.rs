//! A fake EC2 instance metadata service (IMDSv2) for the instance-role tests.

use std::io::{Read, Write};
use std::net::TcpListener;

/// An instance metadata service on `127.0.0.1` that hands out one role with
/// the given keys. It answers on a background thread that ends with the
/// process.
pub struct FakeImds {
    url: String,
}

impl FakeImds {
    /// Starts a service whose role (`test-role`) has these keys.
    pub fn start(access_key: &str, secret_key: &str) -> FakeImds {
        let listener = TcpListener::bind("127.0.0.1:0").expect("FakeImds: bind");
        let url = format!(
            "http://{}",
            listener.local_addr().expect("FakeImds: address")
        );
        let credentials = format!(
            r#"{{"Code":"Success","Type":"AWS-HMAC","AccessKeyId":"{access_key}","SecretAccessKey":"{secret_key}","Token":"fake-token","Expiration":"2099-01-01T00:00:00Z"}}"#
        );
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buffer = [0u8; 4096];
                let Ok(read) = stream.read(&mut buffer) else {
                    continue;
                };
                let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                let mut first = request.lines().next().unwrap_or("").split(' ');
                let method = first.next().unwrap_or("");
                let path = first.next().unwrap_or("");
                let (status, body) = match (method, path) {
                    ("PUT", "/latest/api/token") => (200, "fake-session-token".to_owned()),
                    ("GET", "/latest/meta-data/iam/security-credentials/") => {
                        (200, "test-role".to_owned())
                    }
                    ("GET", "/latest/meta-data/iam/security-credentials/test-role") => {
                        (200, credentials.clone())
                    }
                    _ => (404, String::new()),
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\nx-aws-ec2-metadata-token-ttl-seconds: 21600\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        FakeImds { url }
    }

    /// A metadata service that has no role attached.
    pub fn without_role() -> FakeImds {
        let listener = TcpListener::bind("127.0.0.1:0").expect("FakeImds: bind");
        let url = format!(
            "http://{}",
            listener.local_addr().expect("FakeImds: address")
        );
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buffer = [0u8; 4096];
                let Ok(read) = stream.read(&mut buffer) else {
                    continue;
                };
                let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                let token = request.starts_with("PUT /latest/api/token");
                let (status, body) = if token { (200, "t") } else { (404, "") };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\nx-aws-ec2-metadata-token-ttl-seconds: 21600\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        FakeImds { url }
    }

    /// The base URL (`http://127.0.0.1:port`).
    pub fn url(&self) -> &str {
        &self.url
    }
}
