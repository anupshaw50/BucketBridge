//! Raw HTTP/1.1 endpoints that are slow or silent on purpose, for timeout and
//! stalled-stream tests: behaviour neither s3s nor [`MockS3`](crate::MockS3)
//! can produce, because both answer as fast as they can.
//!
//! Every connection is served by its own thread with blocking sockets, so a
//! deliberately slow connection never holds up another one. Requests must
//! carry a `Content-Length` when they have a body (the SDK always sends one).

use std::io::{ErrorKind as IoErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use bucketbridge_core::model::Account;
use bucketbridge_core::{ClientOptions, Credentials, S3};

use crate::REGION;

/// How often blocked reads wake up to notice that the server is stopping.
const POLL: Duration = Duration::from_millis(100);

/// What a [`RawServer`] does with every request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawBehavior {
    /// Reads whatever arrives and never answers.
    Silent,
    /// Reads the request body at most `chunk` bytes at a time, sleeping
    /// `pause` after each read, and answers `200 OK` (with an `ETag` and an
    /// empty body) once all of it has arrived, as S3 answers an upload.
    SlowDrain {
        /// Largest read.
        chunk: usize,
        /// Sleep after each read.
        pause: Duration,
    },
    /// Reads the request, answers `200 OK` at once and then sends a body of
    /// `chunks` times `chunk` bytes, sleeping `pause` before every chunk but the
    /// first, as a throttled download.
    Trickle {
        /// Bytes per chunk.
        chunk: usize,
        /// Number of chunks.
        chunks: usize,
        /// Sleep between chunks.
        pause: Duration,
    },
}

#[derive(Default)]
struct Stats {
    requests: AtomicUsize,
    body_bytes: AtomicUsize,
}

/// A raw HTTP/1.1 endpoint on `127.0.0.1` behaving as its [`RawBehavior`]
/// says. Dropping it stops accepting connections and ends the connection
/// threads.
pub struct RawServer {
    endpoint: String,
    addr: SocketAddr,
    stats: Arc<Stats>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl RawServer {
    /// Starts the endpoint.
    pub fn start(behavior: RawBehavior) -> RawServer {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("RawServer: bind 127.0.0.1:0");
        let addr = listener.local_addr().expect("RawServer: listener address");
        let stats = Arc::new(Stats::default());
        let stop = Arc::new(AtomicBool::new(false));
        let (thread_stats, thread_stop) = (stats.clone(), stop.clone());
        let accept = std::thread::Builder::new()
            .name(format!("raw-http-{}", addr.port()))
            .spawn(move || {
                for stream in listener.incoming() {
                    if thread_stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    let (stats, stop) = (thread_stats.clone(), thread_stop.clone());
                    let _ = std::thread::Builder::new()
                        .name("raw-http-connection".to_owned())
                        .spawn(move || serve(stream, behavior, &stats, &stop));
                }
            })
            .expect("RawServer: spawn the accept thread");
        RawServer {
            endpoint: format!("http://{addr}"),
            addr,
            stats,
            stop,
            accept: Some(accept),
        }
    }

    /// Endpoint URL.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// An account pointing at the endpoint (custom endpoint, path-style).
    pub fn account(&self) -> Account {
        Account {
            id: "raw-account".to_owned(),
            label: "Raw".to_owned(),
            access_key_id: "AKIDRAW".to_owned(),
            secret_ref: "raw/secret".to_owned(),
            region: REGION.to_owned(),
            endpoint: Some(self.endpoint.clone()),
            path_style: true,
            ..Account::default()
        }
    }

    /// A transport for the endpoint with explicit options.
    pub fn ops_with(&self, options: &ClientOptions) -> S3 {
        let credentials = Credentials::new("AKIDRAW", "raw-secret", None);
        S3::new(&self.account(), &credentials, options).expect("RawServer: build the S3 client")
    }

    /// Requests whose head has been received so far.
    pub fn requests(&self) -> usize {
        self.stats.requests.load(Ordering::SeqCst)
    }

    /// Request body bytes received so far, over all requests.
    pub fn body_bytes(&self) -> usize {
        self.stats.body_bytes.load(Ordering::SeqCst)
    }
}

impl Drop for RawServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so that it sees the flag.
        let _ = TcpStream::connect(self.addr);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

/// Serves requests on one connection until the client closes it or the
/// server stops.
fn serve(mut stream: TcpStream, behavior: RawBehavior, stats: &Stats, stop: &AtomicBool) {
    if stream.set_read_timeout(Some(POLL)).is_err() {
        return;
    }
    let mut pending = Vec::new();
    loop {
        let Some(head) = read_head(&mut stream, &mut pending, stop) else {
            return;
        };
        stats.requests.fetch_add(1, Ordering::SeqCst);
        let body_len = header_value(&head, "content-length")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        let expects_continue =
            header_value(&head, "expect").is_some_and(|v| v.eq_ignore_ascii_case("100-continue"));
        if expects_continue && stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").is_err() {
            return;
        }
        let served = match behavior {
            RawBehavior::Silent => {
                // Swallow everything until the client gives up.
                let _ = read_body(
                    &mut stream,
                    &mut pending,
                    usize::MAX,
                    usize::MAX,
                    Duration::ZERO,
                    stats,
                    stop,
                );
                return;
            }
            RawBehavior::SlowDrain { chunk, pause } => {
                read_body(
                    &mut stream,
                    &mut pending,
                    body_len,
                    chunk.max(1),
                    pause,
                    stats,
                    stop,
                ) && stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nETag: \"raw-etag\"\r\nContent-Length: 0\r\n\r\n",
                    )
                    .is_ok()
            }
            RawBehavior::Trickle {
                chunk,
                chunks,
                pause,
            } => {
                read_body(
                    &mut stream,
                    &mut pending,
                    body_len,
                    64 * 1024,
                    Duration::ZERO,
                    stats,
                    stop,
                ) && trickle(&mut stream, chunk, chunks, pause, stop)
            }
        };
        if !served {
            return;
        }
    }
}

/// Reads up to the end of a request head; `None` when the connection ends
/// first. Bytes after the head stay in `pending`.
fn read_head(stream: &mut TcpStream, pending: &mut Vec<u8>, stop: &AtomicBool) -> Option<String> {
    loop {
        if let Some(end) = pending.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&pending[..end]).into_owned();
            pending.drain(..end + 4);
            return Some(head);
        }
        let mut buf = [0u8; 4096];
        let n = read_some(stream, &mut buf, stop)?;
        pending.extend_from_slice(&buf[..n]);
    }
}

/// Reads `len` body bytes (first from `pending`), at most `chunk` per read,
/// sleeping `pause` after each read. `false` when the connection ends first.
fn read_body(
    stream: &mut TcpStream,
    pending: &mut Vec<u8>,
    len: usize,
    chunk: usize,
    pause: Duration,
    stats: &Stats,
    stop: &AtomicBool,
) -> bool {
    let buffered = pending.len().min(len);
    pending.drain(..buffered);
    stats.body_bytes.fetch_add(buffered, Ordering::SeqCst);
    let mut left = len - buffered;
    let mut buf = vec![0u8; chunk.min(1024 * 1024)];
    while left > 0 {
        let want = left.min(buf.len());
        let Some(n) = read_some(stream, &mut buf[..want], stop) else {
            return false;
        };
        left -= n;
        stats.body_bytes.fetch_add(n, Ordering::SeqCst);
        if !pause.is_zero() {
            std::thread::sleep(pause);
        }
    }
    true
}

/// Answers `200 OK` and sends the body chunk by chunk.
fn trickle(
    stream: &mut TcpStream,
    chunk: usize,
    chunks: usize,
    pause: Duration,
    stop: &AtomicBool,
) -> bool {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nETag: \"raw-etag\"\r\nLast-Modified: Thu, 29 Oct 2026 08:15:00 GMT\r\n\r\n",
        chunk * chunks
    );
    if stream.write_all(head.as_bytes()).is_err() {
        return false;
    }
    let data = vec![b'x'; chunk];
    for index in 0..chunks {
        if index > 0 {
            std::thread::sleep(pause);
        }
        if stop.load(Ordering::SeqCst) || stream.write_all(&data).is_err() {
            return false;
        }
    }
    true
}

/// One read; waits through read timeouts until data arrives, the peer
/// closes the connection (`None`) or the server stops (`None`).
fn read_some(stream: &mut TcpStream, buf: &mut [u8], stop: &AtomicBool) -> Option<usize> {
    loop {
        if stop.load(Ordering::SeqCst) {
            return None;
        }
        match stream.read(buf) {
            Ok(0) => return None,
            Ok(n) => return Some(n),
            Err(err) if matches!(err.kind(), IoErrorKind::WouldBlock | IoErrorKind::TimedOut) => {}
            Err(err) if err.kind() == IoErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
}

/// Value of a header in a request head (name compared case-insensitively).
fn header_value<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_values_are_found_case_insensitively() {
        let head = "PUT /b/k HTTP/1.1\r\nHost: x\r\nContent-Length: 42\r\nexpect: 100-continue";
        assert_eq!(header_value(head, "content-length"), Some("42"));
        assert_eq!(header_value(head, "Expect"), Some("100-continue"));
        assert_eq!(header_value(head, "missing"), None);
        assert_eq!(header_value("GET / HTTP/1.1", "host"), None);
    }
}
