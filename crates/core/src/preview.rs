//! Object previews: a bounded read of the latest version of an object, or of
//! one given version, classified as an image, text, JSON or binary data.
//!
//! [`S3::preview_object`] answers one *page* with one ranged `GetObject`.
//! The object is cut into pages of [`PreviewOptions::page_bytes`] bytes
//! (page `n` starts at byte `n * page_bytes`), so any page of an object of any
//! size costs one request and no more memory than a page:
//!
//! * Page 0 decides what the object is. An image (PNG, JPEG, GIF, WebP, BMP,
//!   ICO, AVIF, and SVG when the content type or the key says so) is returned
//!   whole, as base64, when it fits in [`PreviewOptions::max_image_bytes`];
//!   a larger one is [`PreviewKind::TooLarge`]. UTF-8 text is
//!   [`PreviewKind::Text`], and [`PreviewKind::Json`] when the content type or
//!   the key says JSON; a JSON document that fits in the first page is
//!   validated and pretty-printed with its key order and number spellings
//!   untouched. Anything else is [`PreviewKind::Binary`], and a zero-byte
//!   object is [`PreviewKind::Empty`].
//! * Pages after the first are text. They are cut on character boundaries so
//!   that the pages of an object tile it exactly: a page starts at the first
//!   character that begins at or after its nominal start and ends at the
//!   first character boundary at or after its nominal end.
//!
//! The text is decoded here and never interpreted: a caller shows it as
//! text, never as markup.

use aws_sdk_s3::config::ResponseChecksumValidation;
use aws_smithy_types::error::display::DisplayErrorContext;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::ops::S3;
use crate::s3uri::{validate_bucket_name, validate_key};

/// Bytes per page of text when the caller does not choose (128 KiB).
pub const DEFAULT_PAGE_BYTES: u64 = 128 * 1024;

/// Smallest page size accepted.
pub const MIN_PAGE_BYTES: u64 = 16;

/// Largest page size accepted (1 MiB).
pub const MAX_PAGE_BYTES: u64 = 1024 * 1024;

/// Largest image shown (8 MiB); a bigger one is [`PreviewKind::TooLarge`].
pub const DEFAULT_MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;

/// Largest image budget a caller may ask for (64 MiB).
pub const MAX_IMAGE_BUDGET: u64 = 64 * 1024 * 1024;

/// Bytes read beyond the end of a page so that its last character is whole
/// (a UTF-8 character has at most three continuation bytes).
const LOOKAHEAD: u64 = 3;

/// Bytes of the first page that decide between text and binary data.
const SNIFF_BYTES: usize = 8 * 1024;

/// What the first 1 KiB of an SVG document must contain: its root element.
const SVG_SNIFF_BYTES: usize = 1024;

/// What a preview shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewKind {
    /// UTF-8 text, shown page by page.
    Text,
    /// JSON text: pretty-printed when the whole document fits in the first
    /// page, otherwise shown page by page as it is.
    Json,
    /// An image the web view can draw, returned whole as base64.
    Image,
    /// Data that is neither an image nor UTF-8 text.
    Binary,
    /// An image above the image budget.
    TooLarge,
    /// A zero-byte object.
    Empty,
}

/// How a preview reads an object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewOptions {
    /// Bytes per page of text, [`MIN_PAGE_BYTES`] to [`MAX_PAGE_BYTES`].
    pub page_bytes: u64,
    /// Largest image returned, 1 to [`MAX_IMAGE_BUDGET`] bytes.
    pub max_image_bytes: u64,
}

impl Default for PreviewOptions {
    fn default() -> Self {
        PreviewOptions {
            page_bytes: DEFAULT_PAGE_BYTES,
            max_image_bytes: DEFAULT_MAX_IMAGE_BYTES,
        }
    }
}

impl PreviewOptions {
    /// Options with `page_bytes` and the default image budget (`None` is the
    /// default page size).
    pub fn with_page_bytes(page_bytes: Option<u64>) -> Self {
        PreviewOptions {
            page_bytes: page_bytes.unwrap_or(DEFAULT_PAGE_BYTES),
            ..PreviewOptions::default()
        }
    }

    /// Checks the documented ranges.
    pub fn validate(&self) -> Result<()> {
        if !(MIN_PAGE_BYTES..=MAX_PAGE_BYTES).contains(&self.page_bytes) {
            return Err(Error::Usage(format!(
                "the preview page size must be between {MIN_PAGE_BYTES} and {MAX_PAGE_BYTES} bytes (got {})",
                self.page_bytes
            )));
        }
        if !(1..=MAX_IMAGE_BUDGET).contains(&self.max_image_bytes) {
            return Err(Error::Usage(format!(
                "the image preview limit must be between 1 and {MAX_IMAGE_BUDGET} bytes (got {})",
                self.max_image_bytes
            )));
        }
        Ok(())
    }
}

/// One page of the preview of an object (`ObjectPreview` of
/// `docs/desktop-api.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectPreview {
    /// Object key.
    pub key: String,
    /// The version that was read, when the bucket reports one.
    pub version_id: Option<String>,
    /// What the preview shows.
    pub kind: PreviewKind,
    /// Size of the whole object in bytes.
    pub size: u64,
    /// The `Content-Type` the object is stored with.
    pub content_type: Option<String>,
    /// What the content was recognized as: the image type for an image,
    /// `application/json` for JSON, `text/plain` for other text.
    pub media_type: Option<String>,
    /// Last modification time of the version.
    pub last_modified: Option<DateTime<Utc>>,
    /// Entity tag without quotes.
    pub etag: Option<String>,
    /// The page this answer holds (0 for everything but long text).
    pub page: u64,
    /// How many pages the object has (1 unless it is long text).
    pub page_count: u64,
    /// Bytes per page.
    pub page_size: u64,
    /// First byte of the object shown.
    pub offset: u64,
    /// Bytes of the object shown.
    pub length: u64,
    /// The page as text (text and JSON).
    pub text: Option<String>,
    /// True when JSON was pretty-printed (its text is not the object's bytes).
    pub formatted: bool,
    /// The whole image, base64 (images).
    pub image_base64: Option<String>,
}

impl ObjectPreview {
    fn new(key: &str, kind: PreviewKind, size: u64, options: &PreviewOptions) -> Self {
        ObjectPreview {
            key: key.to_owned(),
            version_id: None,
            kind,
            size,
            content_type: None,
            media_type: None,
            last_modified: None,
            etag: None,
            page: 0,
            page_count: 1,
            page_size: options.page_bytes,
            offset: 0,
            length: 0,
            text: None,
            formatted: false,
            image_base64: None,
        }
    }
}

/// What a ranged read of an object returned.
struct Fetched {
    bytes: Vec<u8>,
    /// Size of the whole object.
    total: u64,
    content_type: Option<String>,
    etag: Option<String>,
    last_modified: Option<DateTime<Utc>>,
    version_id: Option<String>,
}

impl S3 {
    /// Reads page `page` of the preview of an object: its latest version, or
    /// `version_id`. See the [module documentation](self) for how objects are
    /// classified and paged.
    ///
    /// One ranged `GetObject` per page (two for an image longer than the first
    /// page, and one `HeadObject` for an empty object). Fails with
    /// [`crate::ErrorKind::NotFound`] for a missing key or version,
    /// [`crate::ErrorKind::Usage`] for a folder key, a delete marker, an
    /// invalid page size, or a page past the end of the object, and with the
    /// server's own error otherwise (an archived object is a conflict whose
    /// message says to restore it first).
    pub async fn preview_object(
        &self,
        bucket: &str,
        key: &str,
        version_id: Option<&str>,
        page: u64,
        options: &PreviewOptions,
    ) -> Result<ObjectPreview> {
        validate_bucket_name(bucket)?;
        validate_key(key)?;
        if key.ends_with('/') {
            return Err(Error::Usage(format!(
                "s3://{bucket}/{key} is a folder; only objects can be previewed"
            )));
        }
        if version_id.is_some_and(str::is_empty) {
            return Err(Error::Usage("the version ID is empty".into()));
        }
        options.validate()?;
        let (first, last) = page_range(page, options.page_bytes).ok_or_else(|| {
            Error::Usage(format!("page {page} is beyond what an object can hold"))
        })?;
        let fetched = match self
            .ranged_get(bucket, key, version_id, first, last, None)
            .await
        {
            Ok(fetched) => fetched,
            Err(err) if is_unsatisfiable(&err) && page == 0 => {
                return self
                    .empty_preview(bucket, key, version_id, options, err)
                    .await;
            }
            Err(err) if is_unsatisfiable(&err) => {
                return Err(Error::Usage(format!(
                    "page {page} is past the end of s3://{bucket}/{key}"
                )));
            }
            Err(err) => return Err(err),
        };
        if page == 0 {
            self.first_page(bucket, key, version_id, options, fetched)
                .await
        } else {
            later_page(key, page, options, fetched)
        }
    }

    /// The preview of an object whose first page could not be read because it
    /// has no bytes: a `HeadObject` confirms that it is empty.
    async fn empty_preview(
        &self,
        bucket: &str,
        key: &str,
        version_id: Option<&str>,
        options: &PreviewOptions,
        unsatisfiable: Error,
    ) -> Result<ObjectPreview> {
        let head = self.head_object(bucket, key, version_id).await?;
        if head.size != 0 {
            return Err(unsatisfiable);
        }
        let mut preview = ObjectPreview::new(key, PreviewKind::Empty, 0, options);
        preview.version_id = head.version_id.or_else(|| version_id.map(str::to_owned));
        preview.content_type = head.meta.content_type;
        preview.last_modified = head.last_modified;
        preview.etag = head.etag;
        Ok(preview)
    }

    /// Classifies the object from its first page and builds the answer.
    async fn first_page(
        &self,
        bucket: &str,
        key: &str,
        version_id: Option<&str>,
        options: &PreviewOptions,
        mut fetched: Fetched,
    ) -> Result<ObjectPreview> {
        let total = fetched.total;
        let mut preview = ObjectPreview::new(key, PreviewKind::Binary, total, options);
        preview.version_id = fetched
            .version_id
            .clone()
            .or_else(|| version_id.map(str::to_owned));
        preview.content_type = fetched.content_type.clone();
        preview.last_modified = fetched.last_modified;
        preview.etag = fetched.etag.clone();
        if total == 0 {
            preview.kind = PreviewKind::Empty;
            return Ok(preview);
        }
        let whole = u64::try_from(fetched.bytes.len()).is_ok_and(|len| len >= total);
        match classify(fetched.content_type.as_deref(), key, &fetched.bytes, whole) {
            Detected::Image(media_type) => {
                preview.media_type = Some(media_type.to_owned());
                if total > options.max_image_bytes {
                    preview.kind = PreviewKind::TooLarge;
                    return Ok(preview);
                }
                if !whole {
                    self.read_rest(bucket, key, version_id, &mut fetched)
                        .await?;
                }
                preview.kind = PreviewKind::Image;
                preview.length = total;
                preview.image_base64 = Some(BASE64.encode(&fetched.bytes));
            }
            Detected::Text { json } => {
                let (skip, end) = page_slice(&fetched.bytes, options.page_bytes, 0);
                let slice = &fetched.bytes[skip..end];
                let text = String::from_utf8_lossy(slice);
                let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
                preview.page_count = page_count(total, options.page_bytes);
                preview.length = (end - skip) as u64;
                preview.kind = if json {
                    PreviewKind::Json
                } else {
                    PreviewKind::Text
                };
                preview.media_type = Some(
                    if json {
                        "application/json"
                    } else {
                        "text/plain"
                    }
                    .to_owned(),
                );
                if json && whole {
                    match pretty_json(text) {
                        Some(pretty) => {
                            preview.text = Some(pretty);
                            preview.formatted = true;
                        }
                        // Declared JSON that is not JSON: plain text.
                        None => {
                            preview.kind = PreviewKind::Text;
                            preview.media_type = Some("text/plain".to_owned());
                            preview.text = Some(text.to_owned());
                        }
                    }
                } else {
                    preview.text = Some(text.to_owned());
                }
            }
            Detected::Binary => preview.kind = PreviewKind::Binary,
        }
        Ok(preview)
    }

    /// Reads the bytes of an image that the first page did not cover, from
    /// the same version of the object.
    async fn read_rest(
        &self,
        bucket: &str,
        key: &str,
        version_id: Option<&str>,
        fetched: &mut Fetched,
    ) -> Result<()> {
        let have = fetched.bytes.len() as u64;
        let pinned = fetched
            .version_id
            .clone()
            .or_else(|| version_id.map(str::to_owned));
        // The version pins the content on a versioned bucket; the ETag does
        // elsewhere.
        let if_match = pinned.is_none().then(|| fetched.etag.clone()).flatten();
        let rest = self
            .ranged_get(
                bucket,
                key,
                pinned.as_deref(),
                have,
                fetched.total - 1,
                if_match.as_deref(),
            )
            .await?;
        fetched.bytes.extend_from_slice(&rest.bytes);
        if fetched.bytes.len() as u64 != fetched.total {
            return Err(Error::Remote(format!(
                "reading s3://{bucket}/{key}: got {} of {} bytes",
                fetched.bytes.len(),
                fetched.total
            )));
        }
        Ok(())
    }

    /// One ranged `GetObject`: bytes `first` to `last` (inclusive), cut off
    /// at the end of the object.
    async fn ranged_get(
        &self,
        bucket: &str,
        key: &str,
        version_id: Option<&str>,
        first: u64,
        last: u64,
        if_match: Option<&str>,
    ) -> Result<Fetched> {
        let wanted = last - first + 1;
        let client = self.client_for(bucket).await?;
        let out = client
            .get_object()
            .bucket(bucket)
            .key(key)
            .set_version_id(version_id.map(str::to_owned))
            .range(format!("bytes={first}-{last}"))
            .set_if_match(if_match.map(|etag| format!("\"{}\"", etag.trim_matches('"'))))
            .set_request_payer(self.request_payer())
            .customize()
            // A ranged answer has no whole-object checksum to validate.
            .config_override(
                self.metadata_timeout()
                    .response_checksum_validation(ResponseChecksumValidation::WhenRequired),
            )
            .send()
            .await
            .map_err(|err| {
                let err = Error::from(err);
                if version_id.is_some() && is_delete_marker(&err) {
                    Error::Usage(format!(
                        "version {} of s3://{bucket}/{key} is a delete marker; it has no content",
                        version_id.unwrap_or_default()
                    ))
                } else {
                    err.with_context(format!("s3://{bucket}/{key}"))
                }
            })?;
        let range = out.content_range().and_then(parse_content_range);
        let announced = out.content_length().and_then(|len| u64::try_from(len).ok());
        let (start, total) = match range {
            Some((start, _, total)) => (start, total),
            // The whole object, because the server ignored the range.
            None => (0, announced.unwrap_or(0)),
        };
        if start != first {
            return Err(Error::Remote(format!(
                "reading s3://{bucket}/{key}: the server answered from byte {start} instead of byte {first}"
            )));
        }
        let content_type = out.content_type().map(str::to_owned);
        let etag = out.e_tag().map(unquote);
        let last_modified = out
            .last_modified()
            .and_then(|time| DateTime::<Utc>::from_timestamp(time.secs(), time.subsec_nanos()));
        let version_id = out.version_id().map(str::to_owned);
        let limit = usize::try_from(wanted).unwrap_or(usize::MAX);
        let mut body = out.body;
        let mut bytes = Vec::new();
        while bytes.len() < limit {
            let Some(chunk) = body.try_next().await.map_err(|err| {
                Error::Remote(format!(
                    "reading s3://{bucket}/{key}: {}",
                    DisplayErrorContext(&err)
                ))
            })?
            else {
                break;
            };
            let room = limit - bytes.len();
            bytes.extend_from_slice(&chunk[..chunk.len().min(room)]);
        }
        let total = if range.is_some() {
            total
        } else {
            announced.unwrap_or(bytes.len() as u64)
        };
        Ok(Fetched {
            bytes,
            total,
            content_type,
            etag,
            last_modified,
            version_id,
        })
    }
}

/// The answer for a page after the first: text, cut on character boundaries.
fn later_page(
    key: &str,
    page: u64,
    options: &PreviewOptions,
    fetched: Fetched,
) -> Result<ObjectPreview> {
    let json = json_hint(fetched.content_type.as_deref(), key);
    let (skip, end) = page_slice(&fetched.bytes, options.page_bytes, page);
    let mut preview = ObjectPreview::new(
        key,
        if json {
            PreviewKind::Json
        } else {
            PreviewKind::Text
        },
        fetched.total,
        options,
    );
    preview.version_id = fetched.version_id;
    preview.content_type = fetched.content_type;
    preview.last_modified = fetched.last_modified;
    preview.etag = fetched.etag;
    preview.media_type = Some(
        if json {
            "application/json"
        } else {
            "text/plain"
        }
        .to_owned(),
    );
    preview.page = page;
    preview.page_count = page_count(fetched.total, options.page_bytes);
    preview.offset = page * options.page_bytes + skip as u64;
    preview.length = (end - skip) as u64;
    preview.text = Some(String::from_utf8_lossy(&fetched.bytes[skip..end]).into_owned());
    Ok(preview)
}

/// What the first page of an object turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Detected {
    Image(&'static str),
    Text { json: bool },
    Binary,
}

/// Decides what an object is from its stored content type, its key and the
/// start of its content. `whole` says that `sample` is the whole object.
fn classify(content_type: Option<&str>, key: &str, sample: &[u8], whole: bool) -> Detected {
    if let Some(media_type) = sniff_image(sample) {
        return Detected::Image(media_type);
    }
    let svg_hint = content_type.is_some_and(|ct| contains_ignore_case(ct, "svg"))
        || key.to_ascii_lowercase().ends_with(".svg");
    if svg_hint && is_svg(sample) {
        return Detected::Image("image/svg+xml");
    }
    if !looks_like_text(&sample[..sample.len().min(SNIFF_BYTES)], whole) {
        return Detected::Binary;
    }
    let json = json_hint(content_type, key) && starts_like_json(sample);
    Detected::Text { json }
}

/// The content type or the key says JSON.
fn json_hint(content_type: Option<&str>, key: &str) -> bool {
    content_type.is_some_and(|ct| contains_ignore_case(ct, "json"))
        || key.to_ascii_lowercase().ends_with(".json")
}

/// The first character of the document is the start of a JSON object or array.
fn starts_like_json(sample: &[u8]) -> bool {
    strip_bom(sample)
        .iter()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| matches!(byte, b'{' | b'['))
}

fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    haystack
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
}

/// The media type of an image the web view can draw, recognized from its first
/// bytes: PNG, JPEG, GIF, WebP, BMP, ICO and AVIF. (SVG is text; see
/// [`is_svg`].)
pub fn sniff_image(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if head.len() >= 12 && &head[4..8] == b"ftyp" && matches!(&head[8..12], b"avif" | b"avis") {
        return Some("image/avif");
    }
    if is_bmp(head) {
        return Some("image/bmp");
    }
    if is_ico(head) {
        return Some("image/x-icon");
    }
    None
}

/// `BM`, a zero reserved field and a known DIB header size: two letters alone
/// would also match the text "BMW".
fn is_bmp(head: &[u8]) -> bool {
    if head.len() < 18 || &head[..2] != b"BM" || head[6..10] != [0, 0, 0, 0] {
        return false;
    }
    let dib = u32::from_le_bytes([head[14], head[15], head[16], head[17]]);
    matches!(dib, 12 | 40 | 52 | 56 | 64 | 108 | 124)
}

/// An icon directory: type 1, between one and 64 images, a reserved zero in
/// the first entry.
fn is_ico(head: &[u8]) -> bool {
    if head.len() < 10 || head[..4] != [0, 0, 1, 0] {
        return false;
    }
    let count = u16::from_le_bytes([head[4], head[5]]);
    (1..=64).contains(&count) && head[9] == 0
}

/// Whether `head` is an SVG document: after an optional byte order mark,
/// XML declaration, comments and document type, the root element is `<svg`.
pub fn is_svg(head: &[u8]) -> bool {
    let head = strip_bom(&head[..head.len().min(SVG_SNIFF_BYTES)]);
    let text = match std::str::from_utf8(head) {
        Ok(text) => text,
        // The sample may end in the middle of a character.
        Err(err) => std::str::from_utf8(&head[..err.valid_up_to()]).unwrap_or(""),
    };
    let mut rest = text.trim_start();
    loop {
        if let Some(after) = rest.strip_prefix("<?") {
            let Some((_, tail)) = after.split_once("?>") else {
                return false;
            };
            rest = tail.trim_start();
        } else if let Some(after) = rest.strip_prefix("<!--") {
            let Some((_, tail)) = after.split_once("-->") else {
                return false;
            };
            rest = tail.trim_start();
        } else if rest.starts_with("<!") {
            // A document type, possibly with an internal subset in brackets.
            let end = match rest.find('[') {
                Some(open) if rest.find('>').is_none_or(|close| open < close) => {
                    rest.find("]>").map(|at| at + 2)
                }
                _ => rest.find('>').map(|at| at + 1),
            };
            let Some(end) = end else { return false };
            rest = rest[end..].trim_start();
        } else {
            break;
        }
    }
    rest.strip_prefix("<svg")
        .and_then(|tail| tail.chars().next())
        .is_some_and(|next| next.is_ascii_whitespace() || matches!(next, '>' | '/'))
}

/// Whether `sample` is UTF-8 text. `complete` says that it is the whole
/// object, so a character cut off at its end is an error; otherwise a cut
/// character at the end is ignored. A NUL byte, or more than 2 % control
/// characters other than tab, line breaks, form feed and escape, make data
/// binary.
pub fn looks_like_text(sample: &[u8], complete: bool) -> bool {
    let sample = strip_bom(sample);
    if sample.contains(&0) {
        return false;
    }
    let text = match std::str::from_utf8(sample) {
        Ok(text) => text,
        Err(err) if err.error_len().is_none() && !complete => {
            match std::str::from_utf8(&sample[..err.valid_up_to()]) {
                Ok(text) => text,
                Err(_) => return false,
            }
        }
        Err(_) => return false,
    };
    let (mut chars, mut control) = (0usize, 0usize);
    for ch in text.chars() {
        chars += 1;
        if (ch.is_control() && !matches!(ch, '\t' | '\n' | '\r' | '\u{c}' | '\u{1b}'))
            || ch == '\u{7f}'
        {
            control += 1;
        }
    }
    control * 50 <= chars
}

/// Validates `text` as JSON and re-indents it with two spaces, keeping the
/// order of object members and the spelling of strings and numbers exactly as
/// they are. `None` when `text` is not one JSON document.
pub fn pretty_json(text: &str) -> Option<String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    serde_json::from_str::<serde::de::IgnoredAny>(text).ok()?;
    Some(reindent(text))
}

/// Re-indents valid JSON text.
fn reindent(text: &str) -> String {
    fn line_break(out: &mut String, depth: usize) {
        out.push('\n');
        for _ in 0..depth {
            out.push_str("  ");
        }
    }
    let mut out = String::with_capacity(text.len() + text.len() / 2);
    let mut depth = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                out.push('"');
                let mut escaped = false;
                for inner in chars.by_ref() {
                    out.push(inner);
                    if escaped {
                        escaped = false;
                    } else if inner == '\\' {
                        escaped = true;
                    } else if inner == '"' {
                        break;
                    }
                }
            }
            '{' | '[' => {
                let close = if ch == '{' { '}' } else { ']' };
                out.push(ch);
                while chars.next_if(|next| next.is_whitespace()).is_some() {}
                if chars.next_if_eq(&close).is_some() {
                    // An empty object or array stays on one line.
                    out.push(close);
                } else {
                    depth += 1;
                    line_break(&mut out, depth);
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                line_break(&mut out, depth);
                out.push(ch);
            }
            ',' => {
                out.push(',');
                line_break(&mut out, depth);
            }
            ':' => out.push_str(": "),
            ch if ch.is_whitespace() => {}
            ch => out.push(ch),
        }
    }
    out
}

/// A UTF-8 continuation byte (`10xxxxxx`).
fn is_continuation(byte: u8) -> bool {
    byte & 0xC0 == 0x80
}

/// The bytes to request for page `page`: its window plus the look-ahead, as
/// the inclusive range `first..=last`. `None` when the arithmetic overflows.
fn page_range(page: u64, page_bytes: u64) -> Option<(u64, u64)> {
    let first = page.checked_mul(page_bytes)?;
    let last = first.checked_add(page_bytes + LOOKAHEAD - 1)?;
    // The last byte S3 can address is below 2^63.
    (last < i64::MAX as u64).then_some((first, last))
}

/// How many pages an object of `total` bytes has (at least one).
fn page_count(total: u64, page_bytes: u64) -> u64 {
    total.div_ceil(page_bytes).max(1)
}

/// Which part of the bytes fetched for page `page` belongs to it: from the
/// first character that begins in the page's window (a page after the first
/// skips the tail of the character the page before it ends with) to the end
/// of the character that crosses the window's end. Consecutive pages tile the
/// object exactly.
fn page_slice(bytes: &[u8], page_bytes: u64, page: u64) -> (usize, usize) {
    let window = usize::try_from(page_bytes)
        .unwrap_or(usize::MAX)
        .min(bytes.len());
    let skip = if page == 0 {
        0
    } else {
        bytes
            .iter()
            .take(LOOKAHEAD as usize)
            .take_while(|byte| is_continuation(**byte))
            .count()
    };
    let mut end = window;
    while end < bytes.len() && end < window + LOOKAHEAD as usize && is_continuation(bytes[end]) {
        end += 1;
    }
    (skip.min(end), end)
}

/// `bytes first-last/total`.
fn parse_content_range(value: &str) -> Option<(u64, u64, u64)> {
    let rest = value.trim().strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let (first, last) = range.split_once('-')?;
    Some((
        first.trim().parse().ok()?,
        last.trim().parse().ok()?,
        total.trim().parse().ok()?,
    ))
}

/// A range that starts at or past the end of the object (HTTP 416): what
/// S3 answers for any range of an empty object.
fn is_unsatisfiable(err: &Error) -> bool {
    err.service().is_some_and(|service| {
        service.status() == Some(416) || service.code() == Some("InvalidRange")
    })
}

/// `GetObject` of a version that is a delete marker (HTTP 405).
fn is_delete_marker(err: &Error) -> bool {
    err.service().is_some_and(|service| {
        service.status() == Some(405) || service.code() == Some("MethodNotAllowed")
    })
}

fn unquote(etag: &str) -> String {
    etag.strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(etag)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal WebP: `RIFF`, a size, `WEBP` and a lossless chunk.
    const WEBP: &[u8] = b"RIFF\x1a\x00\x00\x00WEBPVP8L\x0d\x00\x00\x00\x2f\x00\x00\x00\x10\x07\x10\x11\x11\x88\x88\xfe\x07\x00";

    #[test]
    fn images_are_recognized_by_their_first_bytes() {
        assert_eq!(
            sniff_image(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"),
            Some("image/png")
        );
        assert_eq!(
            sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 16]),
            Some("image/jpeg")
        );
        assert_eq!(sniff_image(b"GIF89a\x01\x00"), Some("image/gif"));
        assert_eq!(sniff_image(b"GIF87a\x01\x00"), Some("image/gif"));
        assert_eq!(sniff_image(WEBP), Some("image/webp"));
        assert_eq!(
            sniff_image(b"\x00\x00\x00\x1cftypavif\x00\x00\x00\x00"),
            Some("image/avif")
        );
        let mut bmp = b"BM\x36\x00\x00\x00\x00\x00\x00\x00\x36\x00\x00\x00".to_vec();
        bmp.extend_from_slice(&40u32.to_le_bytes());
        assert_eq!(sniff_image(&bmp), Some("image/bmp"));
        assert_eq!(
            sniff_image(&[0, 0, 1, 0, 1, 0, 16, 16, 0, 0, 1, 0, 32, 0]),
            Some("image/x-icon")
        );
    }

    #[test]
    fn look_alikes_and_short_samples_are_not_images() {
        for text in [
            &b"BMW makes cars and this is a sentence"[..],
            b"RIFF....WAVEfmt ",
            b"RIFF",
            b"\x89PNG",
            b"GIF90a",
            b"",
            b"plain text",
        ] {
            assert_eq!(sniff_image(text), None, "{text:?}");
        }
        // A zero-count icon directory and a RIFF that is not WebP.
        assert_eq!(sniff_image(&[0, 0, 1, 0, 0, 0, 16, 16, 0, 0]), None);
        assert_eq!(sniff_image(b"RIFF\x1a\x00\x00\x00AVI LIST"), None);
    }

    #[test]
    fn svg_needs_an_svg_root_element() {
        for ok in [
            "<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
            "<?xml version=\"1.0\"?>\n<!-- logo -->\n<svg viewBox=\"0 0 1 1\">",
            "\u{feff}  <svg>",
            "<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd\">\n<svg>",
            "<!DOCTYPE svg [ <!ENTITY a \"b\"> ]>\n<svg>",
        ] {
            assert!(is_svg(ok.as_bytes()), "{ok}");
        }
        for bad in [
            "<html><svg></svg></html>",
            "<svgfoo>",
            "<svg",
            "text <svg>",
            "<?xml version=\"1.0\"",
            "<!-- never closed <svg>",
            "",
        ] {
            assert!(!is_svg(bad.as_bytes()), "{bad}");
        }
    }

    #[test]
    fn text_is_utf8_without_nul_and_with_few_control_characters() {
        assert!(looks_like_text(b"hello\nworld\ttab\r\n", true));
        assert!(looks_like_text(
            "caf\u{e9} \u{65e5}\u{672c}".as_bytes(),
            true
        ));
        assert!(looks_like_text(b"\xEF\xBB\xBFwith a byte order mark", true));
        assert!(looks_like_text(b"\x1b[31mred\x1b[0m", true));
        assert!(!looks_like_text(b"nul\0byte", true));
        assert!(!looks_like_text(&[0xFF, 0xFE, b'a'], true));
        assert!(!looks_like_text(b"\x01\x02\x03\x04\x05\x06abc", true));
        // A character cut off at the end of a sample is fine only when the
        // sample is not the whole object.
        let cut = &"\u{65e5}".as_bytes()[..2];
        assert!(looks_like_text(cut, false));
        assert!(!looks_like_text(cut, true));
        // The last 20 characters are fine; 3 control characters in 100 are not.
        let mut mostly = vec![b'a'; 97];
        mostly.extend_from_slice(&[1, 2, 3]);
        assert!(!looks_like_text(&mostly, true));
        mostly.truncate(98);
        mostly.push(b' ');
        mostly.push(b' ');
        assert!(looks_like_text(&mostly, true));
    }

    #[test]
    fn classification_follows_the_content_not_the_label() {
        let png = b"\x89PNG\r\n\x1a\n";
        assert_eq!(
            classify(Some("text/plain"), "a.txt", png, true),
            Detected::Image("image/png")
        );
        assert_eq!(
            classify(Some("image/png"), "a.png", b"not an image", true),
            Detected::Text { json: false }
        );
        assert_eq!(
            classify(None, "a.bin", &[0, 1, 2, 3], true),
            Detected::Binary
        );
        assert_eq!(
            classify(Some("application/json"), "a", b"  {\"a\":1}", true),
            Detected::Text { json: true }
        );
        assert_eq!(
            classify(Some("application/octet-stream"), "data.JSON", b"[1]", true),
            Detected::Text { json: true }
        );
        // JSON by label, but the content is not JSON-shaped.
        assert_eq!(
            classify(Some("application/json"), "a.json", b"hello", true),
            Detected::Text { json: false }
        );
        assert_eq!(
            classify(Some("text/plain"), "a.json", b"\xEF\xBB\xBF{\"a\":1}", true),
            Detected::Text { json: true }
        );
        // SVG needs a hint besides its root element.
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>";
        assert_eq!(
            classify(Some("image/svg+xml"), "logo", svg, true),
            Detected::Image("image/svg+xml")
        );
        assert_eq!(
            classify(None, "logo.SVG", svg, true),
            Detected::Image("image/svg+xml")
        );
        assert_eq!(
            classify(Some("text/plain"), "logo.txt", svg, true),
            Detected::Text { json: false }
        );
    }

    #[test]
    fn json_is_validated_and_indented_without_reordering() {
        let pretty =
            pretty_json(r#"{"b":1.0,"a":[1,2,{"x":null}],"e":{},"f":[ ],"s":"a,b:{c}\"d\\"}"#)
                .unwrap();
        assert_eq!(
            pretty,
            "{\n  \"b\": 1.0,\n  \"a\": [\n    1,\n    2,\n    {\n      \"x\": null\n    }\n  ],\n  \"e\": {},\n  \"f\": [],\n  \"s\": \"a,b:{c}\\\"d\\\\\"\n}"
        );
        assert_eq!(pretty_json("[1, 2]").unwrap(), "[\n  1,\n  2\n]");
        assert_eq!(pretty_json("  42 ").unwrap(), "42");
        assert_eq!(pretty_json("\u{feff}{}").unwrap(), "{}");
        assert_eq!(
            pretty_json("{\"\\u00e9\":\"\u{e9}\"}").unwrap(),
            "{\n  \"\\u00e9\": \"\u{e9}\"\n}"
        );
        // Not JSON: truncated, trailing data, empty, plain text.
        for bad in ["{\"a\":", "[1,]", "{} {}", "", "hello", "{'a':1}"] {
            assert_eq!(pretty_json(bad), None, "{bad}");
        }
        // The result is itself valid and means the same.
        let value: serde_json::Value = serde_json::from_str(&pretty).unwrap();
        assert_eq!(value["a"][2]["x"], serde_json::Value::Null);
    }

    #[test]
    fn page_windows_include_the_lookahead() {
        assert_eq!(page_range(0, 100), Some((0, 102)));
        assert_eq!(page_range(3, 100), Some((300, 402)));
        assert_eq!(page_range(u64::MAX, 100), None);
        // Offsets S3 cannot address (it has no byte past 2^63 - 1) are refused.
        assert_eq!(page_range(i64::MAX as u64 / 100, 100), None);
        assert_eq!(
            page_range(1_000_000_000_000_000, 100),
            Some((100_000_000_000_000_000, 100_000_000_000_000_102))
        );
        assert_eq!(page_count(0, 100), 1);
        assert_eq!(page_count(1, 100), 1);
        assert_eq!(page_count(100, 100), 1);
        assert_eq!(page_count(101, 100), 2);
        assert_eq!(page_count(1000, 100), 10);
    }

    /// Splits `data` into pages the way [`later_page`] and the first page do,
    /// from the bytes a ranged read returns.
    fn pages_of(data: &[u8], page_bytes: u64) -> Vec<String> {
        let count = page_count(data.len() as u64, page_bytes);
        (0..count)
            .map(|page| {
                let (first, last) = page_range(page, page_bytes).unwrap();
                let end = (last as usize + 1).min(data.len());
                let fetched = &data[first as usize..end];
                let (skip, take) = page_slice(fetched, page_bytes, page);
                String::from_utf8_lossy(&fetched[skip..take]).into_owned()
            })
            .collect()
    }

    #[test]
    fn pages_tile_text_of_any_script_exactly() {
        let samples = [
            "plain ascii text that is a little longer than a few pages of sixteen bytes".to_owned(),
            "caf\u{e9} cr\u{e8}me br\u{fb}l\u{e9}e \u{65e5}\u{672c}\u{8a9e}\u{306e}\u{30c6}\u{30ad}\u{30b9}\u{30c8} \u{1f4f7}\u{1f680} end".to_owned(),
            "\u{1f600}".repeat(40),
            "\u{65e5}".repeat(33),
            String::new(),
            "x".to_owned(),
        ];
        for sample in &samples {
            for page_bytes in [16u64, 17, 18, 19, 20, 33] {
                let pages = pages_of(sample.as_bytes(), page_bytes);
                assert_eq!(
                    pages.concat(),
                    *sample,
                    "page size {page_bytes}: {sample:?}"
                );
                for page in &pages {
                    // Whole characters only: nothing was replaced.
                    assert!(!page.contains('\u{fffd}'), "{page:?}");
                }
            }
        }
    }

    #[test]
    fn a_page_reports_where_it_starts_and_how_long_it_is() {
        // "\u{e9}" is two bytes. With 16-byte pages, byte 16 is the second
        // byte of a character that began on page 0.
        let data = format!("{}{}tail", "a".repeat(15), "\u{e9}");
        let bytes = data.as_bytes();
        assert_eq!(bytes.len(), 21);
        let (first, last) = page_range(0, 16).unwrap();
        let fetched = &bytes[first as usize..(last as usize + 1).min(bytes.len())];
        assert_eq!(page_slice(fetched, 16, 0), (0, 17));
        let (first, last) = page_range(1, 16).unwrap();
        let fetched = &bytes[first as usize..(last as usize + 1).min(bytes.len())];
        // Its first byte is the tail of the character that page 0 ended with.
        assert_eq!(page_slice(fetched, 16, 1), (1, 5));
        // A window that opens inside a character skips its tail.
        let split = "\u{65e5}\u{672c}".as_bytes();
        assert_eq!(page_slice(&split[1..], 16, 1), (2, 5));
    }

    #[test]
    fn content_ranges_parse() {
        assert_eq!(parse_content_range("bytes 0-99/1000"), Some((0, 99, 1000)));
        assert_eq!(parse_content_range(" bytes 5-5/6 "), Some((5, 5, 6)));
        for bad in [
            "bytes */1000",
            "items 0-1/2",
            "bytes 0-1/*",
            "bytes a-b/c",
            "",
        ] {
            assert_eq!(parse_content_range(bad), None, "{bad}");
        }
    }

    #[test]
    fn options_are_validated() {
        PreviewOptions::default().validate().unwrap();
        assert_eq!(
            PreviewOptions::with_page_bytes(None),
            PreviewOptions::default()
        );
        for page_bytes in [0, MIN_PAGE_BYTES - 1, MAX_PAGE_BYTES + 1] {
            let options = PreviewOptions::with_page_bytes(Some(page_bytes));
            assert_eq!(
                options.validate().unwrap_err().kind(),
                crate::ErrorKind::Usage,
                "{page_bytes}"
            );
        }
        for max_image_bytes in [0, MAX_IMAGE_BUDGET + 1] {
            let options = PreviewOptions {
                max_image_bytes,
                ..PreviewOptions::default()
            };
            assert!(options.validate().is_err(), "{max_image_bytes}");
        }
        PreviewOptions::with_page_bytes(Some(MIN_PAGE_BYTES))
            .validate()
            .unwrap();
        PreviewOptions::with_page_bytes(Some(MAX_PAGE_BYTES))
            .validate()
            .unwrap();
    }

    #[test]
    fn the_wire_shape_is_stable() {
        let preview = ObjectPreview {
            key: "a/b.png".into(),
            version_id: Some("v1".into()),
            kind: PreviewKind::TooLarge,
            size: 9,
            content_type: Some("image/png".into()),
            media_type: Some("image/png".into()),
            last_modified: None,
            etag: Some("e".into()),
            page: 0,
            page_count: 1,
            page_size: 131_072,
            offset: 0,
            length: 0,
            text: None,
            formatted: false,
            image_base64: None,
        };
        let value = serde_json::to_value(&preview).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "key": "a/b.png",
                "version_id": "v1",
                "kind": "too_large",
                "size": 9,
                "content_type": "image/png",
                "media_type": "image/png",
                "last_modified": null,
                "etag": "e",
                "page": 0,
                "page_count": 1,
                "page_size": 131_072,
                "offset": 0,
                "length": 0,
                "text": null,
                "formatted": false,
                "image_base64": null
            })
        );
        assert_eq!(
            serde_json::from_value::<ObjectPreview>(value).unwrap(),
            preview
        );
        for (kind, text) in [
            (PreviewKind::Text, "text"),
            (PreviewKind::Json, "json"),
            (PreviewKind::Image, "image"),
            (PreviewKind::Binary, "binary"),
            (PreviewKind::TooLarge, "too_large"),
            (PreviewKind::Empty, "empty"),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), serde_json::json!(text));
        }
    }
}
