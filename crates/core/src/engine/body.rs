//! Request bodies that stream a byte range of an upload source.
//!
//! A body reads the range in chunks of [`READ_CHUNK`] bytes on the blocking
//! thread pool (positional reads, so nothing outside the range is read and
//! no file is ever loaded whole), and hands each chunk to the connection in
//! pieces of at most [`Throttle::max_grant`] bytes, each after a grant of
//! the throttle. The SDK may retry a request; the body is rebuilt from the
//! source for every attempt, so a retry re-reads the range.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, ready};

use aws_sdk_s3::primitives::ByteStream;
use aws_smithy_types::body::SdkBody;
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use tokio::task::JoinHandle;

use super::control::{Progress, Throttle};
use super::source::UploadSource;

/// Bytes read from the source at a time.
pub(crate) const READ_CHUNK: u64 = 256 * 1024;

/// Counts the payload of one request as it is handed to the connection.
///
/// When the SDK retries the request, the rebuilt body streams the same bytes
/// again; they are counted only once (the counter only ever moves forward to
/// the largest amount any attempt has sent).
pub(crate) struct SentCounter {
    counted: AtomicU64,
    progress: Arc<dyn Progress>,
}

impl SentCounter {
    pub(crate) fn new(progress: Arc<dyn Progress>) -> Arc<Self> {
        Arc::new(SentCounter {
            counted: AtomicU64::new(0),
            progress,
        })
    }

    /// One attempt has sent `sent` bytes in total.
    fn record(&self, sent: u64) {
        let previous = self.counted.fetch_max(sent, Ordering::AcqRel);
        if sent > previous {
            self.progress.add_done(sent - previous);
        }
    }

    /// The request succeeded with `total` bytes; counts what is left.
    pub(crate) fn finish(&self, total: u64) {
        self.record(total);
    }
}

/// A retryable request body with `len` bytes of `source` from `offset`.
pub(crate) fn range_body(
    source: Arc<dyn UploadSource>,
    offset: u64,
    len: u64,
    throttle: Arc<dyn Throttle>,
    counter: Arc<SentCounter>,
) -> ByteStream {
    if len == 0 {
        return ByteStream::from_static(b"");
    }
    let rebuild = move || {
        SdkBody::from_body_1_x(RangeBody::new(
            Arc::clone(&source),
            offset,
            len,
            Arc::clone(&throttle),
            Arc::clone(&counter),
        ))
    };
    ByteStream::new(SdkBody::retryable(rebuild))
}

/// The body of one attempt. The mutex only makes the type `Sync` (which the
/// SDK requires of bodies); `poll_frame` has exclusive access and never
/// locks it.
struct RangeBody {
    state: Mutex<RangeState>,
}

struct RangeState {
    source: Arc<dyn UploadSource>,
    throttle: Arc<dyn Throttle>,
    counter: Arc<SentCounter>,
    /// Next offset to read from the source.
    offset: u64,
    /// Bytes not yet read from the source.
    unread: u64,
    /// Bytes handed to the connection by this attempt.
    sent: u64,
    /// Total length of the range.
    len: u64,
    /// Read but not yet handed to the connection.
    pending: Bytes,
    step: Step,
}

enum Step {
    /// Ready to hand out the next piece (or to read the next chunk).
    Idle,
    /// Waiting for the throttle to grant `len` bytes of `pending`.
    Granting {
        len: usize,
        grant: Pin<Box<dyn Future<Output = ()> + Send>>,
    },
    /// Reading the next chunk from the source.
    Reading {
        len: u64,
        read: JoinHandle<io::Result<Vec<u8>>>,
    },
    /// A read failed; the body is finished.
    Failed,
}

impl RangeBody {
    fn new(
        source: Arc<dyn UploadSource>,
        offset: u64,
        len: u64,
        throttle: Arc<dyn Throttle>,
        counter: Arc<SentCounter>,
    ) -> Self {
        RangeBody {
            state: Mutex::new(RangeState {
                source,
                throttle,
                counter,
                offset,
                unread: len,
                sent: 0,
                len,
                pending: Bytes::new(),
                step: Step::Idle,
            }),
        }
    }

    fn remaining(&self) -> u64 {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.len - state.sent
    }
}

impl RangeState {
    fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<io::Result<Frame<Bytes>>>> {
        loop {
            match &mut self.step {
                Step::Failed => return Poll::Ready(None),
                Step::Idle => {
                    if !self.pending.is_empty() {
                        let max = self.throttle.max_grant().max(1);
                        let len = usize::try_from(max)
                            .unwrap_or(usize::MAX)
                            .min(self.pending.len());
                        let throttle = Arc::clone(&self.throttle);
                        let bytes = len as u64;
                        self.step = Step::Granting {
                            len,
                            grant: Box::pin(async move { throttle.acquire(bytes).await }),
                        };
                    } else if self.unread > 0 {
                        let len = self.unread.min(READ_CHUNK);
                        let source = Arc::clone(&self.source);
                        let offset = self.offset;
                        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                            self.step = Step::Failed;
                            return Poll::Ready(Some(Err(io::Error::other(
                                "the upload body needs a Tokio runtime to read its source",
                            ))));
                        };
                        let read = runtime.spawn_blocking(move || {
                            let mut buf =
                                vec![0u8; usize::try_from(len).map_err(io::Error::other)?];
                            source.read_exact_at(offset, &mut buf).map(|()| buf)
                        });
                        self.step = Step::Reading { len, read };
                    } else {
                        return Poll::Ready(None);
                    }
                }
                Step::Granting { len, grant } => {
                    ready!(grant.as_mut().poll(cx));
                    let len = *len;
                    self.step = Step::Idle;
                    let piece = self.pending.split_to(len);
                    self.sent += len as u64;
                    self.counter.record(self.sent);
                    return Poll::Ready(Some(Ok(Frame::data(piece))));
                }
                Step::Reading { len, read } => {
                    let result = ready!(Pin::new(read).poll(cx));
                    let len = *len;
                    match result {
                        Ok(Ok(data)) => {
                            self.offset += len;
                            self.unread -= len;
                            self.pending = Bytes::from(data);
                            self.step = Step::Idle;
                        }
                        Ok(Err(err)) => {
                            self.step = Step::Failed;
                            return Poll::Ready(Some(Err(err)));
                        }
                        Err(err) => {
                            self.step = Step::Failed;
                            return Poll::Ready(Some(Err(io::Error::other(format!(
                                "reading the upload source failed: {err}"
                            )))));
                        }
                    }
                }
            }
        }
    }
}

impl Body for RangeBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        self.get_mut()
            .state
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .poll_next(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.remaining() == 0
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.remaining())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::control::{ProgressCounter, ThrottleFuture};
    use crate::engine::source::FileSource;
    use std::sync::atomic::AtomicUsize;

    #[derive(Default)]
    struct CountingThrottle {
        bytes: AtomicU64,
        calls: AtomicUsize,
        max: u64,
    }

    impl Throttle for CountingThrottle {
        fn acquire(&self, bytes: u64) -> ThrottleFuture<'_> {
            assert!(bytes <= self.max, "grant of {bytes} > {}", self.max);
            self.bytes.fetch_add(bytes, Ordering::SeqCst);
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                tokio::task::yield_now().await;
            })
        }

        fn max_grant(&self) -> u64 {
            self.max
        }
    }

    fn source(len: usize) -> (tempfile::TempDir, Vec<u8>, Arc<dyn UploadSource>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("body.bin");
        let data: Vec<u8> = (0..len).map(|i| (i * 7 % 256) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        let source = Arc::new(FileSource::open(&path).unwrap());
        (dir, data, source)
    }

    #[tokio::test]
    async fn streams_the_range_through_the_throttle_and_retries_from_scratch() {
        let (_dir, data, source) = source(700_000);
        let throttle = Arc::new(CountingThrottle {
            max: 100_000,
            ..CountingThrottle::default()
        });
        let progress = Arc::new(ProgressCounter::new());
        let counter = SentCounter::new(progress.clone());
        let (offset, len) = (12_345u64, 600_000u64);
        let body = range_body(source, offset, len, throttle.clone(), Arc::clone(&counter));
        let sdk_body = body.into_inner();
        assert_eq!(sdk_body.content_length(), Some(len));
        let retry = sdk_body.try_clone().expect("the body is retryable");

        let first = ByteStream::new(sdk_body).collect().await.unwrap().to_vec();
        let expected = &data[offset as usize..(offset + len) as usize];
        assert_eq!(first, expected);
        assert_eq!(throttle.bytes.load(Ordering::SeqCst), len);
        assert!(throttle.calls.load(Ordering::SeqCst) >= 6);
        assert_eq!(progress.done(), len);

        // A retry streams the same bytes again; they use the throttle again
        // but are not counted as progress twice.
        let second = ByteStream::new(retry).collect().await.unwrap().to_vec();
        assert_eq!(second, expected);
        assert_eq!(throttle.bytes.load(Ordering::SeqCst), 2 * len);
        assert_eq!(progress.done(), len);
        counter.finish(len);
        assert_eq!(progress.done(), len);
    }

    #[tokio::test]
    async fn a_short_source_fails_the_body() {
        let (_dir, _data, source) = source(1000);
        let counter = SentCounter::new(Arc::new(ProgressCounter::new()));
        let body = range_body(
            source,
            500,
            1000,
            Arc::new(crate::engine::control::Unthrottled),
            counter,
        );
        let err = body.collect().await.unwrap_err();
        let mut current: Option<&(dyn std::error::Error + 'static)> = Some(&err);
        let mut short_read = false;
        while let Some(error) = current {
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|io| io.kind() == io::ErrorKind::UnexpectedEof)
            {
                short_read = true;
            }
            current = error.source();
        }
        assert!(short_read, "{err:?}");
    }

    #[tokio::test]
    async fn empty_ranges_are_empty_bodies() {
        let (_dir, _data, source) = source(10);
        let counter = SentCounter::new(Arc::new(ProgressCounter::new()));
        let body = range_body(
            source,
            0,
            0,
            Arc::new(crate::engine::control::Unthrottled),
            counter,
        );
        assert_eq!(body.collect().await.unwrap().to_vec(), Vec::<u8>::new());
    }
}
