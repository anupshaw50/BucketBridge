//! `Content-Type` for uploads, guessed from the file extension.

use std::path::Path;

/// The type used when the extension is unknown or missing.
pub const DEFAULT_CONTENT_TYPE: &str = "application/octet-stream";

/// Extension (lowercase) to IANA media type.
const TYPES: &[(&str, &str)] = &[
    // Text and documents.
    ("txt", "text/plain"),
    ("text", "text/plain"),
    ("log", "text/plain"),
    ("md", "text/markdown"),
    ("markdown", "text/markdown"),
    ("csv", "text/csv"),
    ("tsv", "text/tab-separated-values"),
    ("htm", "text/html"),
    ("html", "text/html"),
    ("css", "text/css"),
    ("js", "text/javascript"),
    ("mjs", "text/javascript"),
    ("ics", "text/calendar"),
    ("vtt", "text/vtt"),
    ("xml", "application/xml"),
    ("json", "application/json"),
    ("map", "application/json"),
    ("jsonld", "application/ld+json"),
    ("ndjson", "application/x-ndjson"),
    ("yaml", "application/yaml"),
    ("yml", "application/yaml"),
    ("toml", "application/toml"),
    ("pdf", "application/pdf"),
    ("rtf", "application/rtf"),
    ("epub", "application/epub+zip"),
    ("doc", "application/msword"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    ("xls", "application/vnd.ms-excel"),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    ("ppt", "application/vnd.ms-powerpoint"),
    (
        "pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
    ("odt", "application/vnd.oasis.opendocument.text"),
    ("ods", "application/vnd.oasis.opendocument.spreadsheet"),
    ("odp", "application/vnd.oasis.opendocument.presentation"),
    // Archives and binary formats.
    ("zip", "application/zip"),
    ("gz", "application/gzip"),
    ("tgz", "application/gzip"),
    ("bz2", "application/x-bzip2"),
    ("xz", "application/x-xz"),
    ("zst", "application/zstd"),
    ("7z", "application/x-7z-compressed"),
    ("rar", "application/vnd.rar"),
    ("tar", "application/x-tar"),
    ("jar", "application/java-archive"),
    ("wasm", "application/wasm"),
    ("parquet", "application/vnd.apache.parquet"),
    ("avro", "application/avro"),
    ("sql", "application/sql"),
    ("sh", "application/x-sh"),
    // Images.
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("avif", "image/avif"),
    ("heic", "image/heic"),
    ("heif", "image/heif"),
    ("bmp", "image/bmp"),
    ("tif", "image/tiff"),
    ("tiff", "image/tiff"),
    ("ico", "image/vnd.microsoft.icon"),
    ("svg", "image/svg+xml"),
    // Audio.
    ("mp3", "audio/mpeg"),
    ("wav", "audio/wav"),
    ("ogg", "audio/ogg"),
    ("oga", "audio/ogg"),
    ("opus", "audio/opus"),
    ("flac", "audio/flac"),
    ("aac", "audio/aac"),
    ("m4a", "audio/mp4"),
    ("mid", "audio/midi"),
    ("midi", "audio/midi"),
    // Video.
    ("mp4", "video/mp4"),
    ("m4v", "video/mp4"),
    ("mov", "video/quicktime"),
    ("webm", "video/webm"),
    ("mkv", "video/x-matroska"),
    ("avi", "video/x-msvideo"),
    ("mpeg", "video/mpeg"),
    ("mpg", "video/mpeg"),
    ("ogv", "video/ogg"),
    ("m3u8", "application/vnd.apple.mpegurl"),
    // Fonts.
    ("woff", "font/woff"),
    ("woff2", "font/woff2"),
    ("ttf", "font/ttf"),
    ("otf", "font/otf"),
];

/// Guesses the `Content-Type` of a file from its extension (compared
/// without regard to ASCII case); [`DEFAULT_CONTENT_TYPE`] when the
/// extension is unknown, missing or not valid UTF-8.
pub fn guess_content_type(path: &Path) -> &'static str {
    let Some(extension) = path.extension().and_then(|e| e.to_str()) else {
        return DEFAULT_CONTENT_TYPE;
    };
    TYPES
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(extension))
        .map_or(DEFAULT_CONTENT_TYPE, |(_, media_type)| media_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_extensions() {
        for (name, expected) in [
            ("a.txt", "text/plain"),
            ("dir/photo.JPG", "image/jpeg"),
            ("archive.tar.gz", "application/gzip"),
            ("page.Html", "text/html"),
            ("data.json", "application/json"),
            ("clip.webm", "video/webm"),
            ("doc.pdf", "application/pdf"),
        ] {
            assert_eq!(guess_content_type(Path::new(name)), expected, "{name}");
        }
    }

    #[test]
    fn unknown_or_missing_extensions_are_octet_streams() {
        for name in [
            "noext",
            ".bashrc",
            "a.unknownext",
            "trailing.",
            "dir.d/file",
        ] {
            assert_eq!(
                guess_content_type(Path::new(name)),
                DEFAULT_CONTENT_TYPE,
                "{name}"
            );
        }
    }

    #[test]
    fn table_is_consistent() {
        let mut seen = std::collections::BTreeSet::new();
        for (extension, media_type) in TYPES {
            assert_eq!(*extension, extension.to_ascii_lowercase());
            assert!(seen.insert(*extension), "duplicate {extension}");
            assert!(media_type.contains('/'), "{media_type}");
        }
    }
}
