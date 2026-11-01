//! Command-line locations: S3 URIs, bucket arguments, local paths, and where
//! copied objects land (the folder semantics of the IPC contract).

use std::fmt;
use std::path::PathBuf;

use bucketbridge_core::path::{join_key, parent_prefix, strip_parent};
use bucketbridge_core::{Error, Result, S3Uri};

/// A `cp`/`mv` endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// A local file or folder.
    Local(PathBuf),
    /// An object, a prefix or a bucket.
    S3(S3Uri),
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Location::Local(path) => write!(f, "{}", path.display()),
            Location::S3(uri) => write!(f, "{uri}"),
        }
    }
}

/// Parses an argument that must be an S3 URI (`ls`, `rm`, `presign`).
pub fn parse_s3_uri(text: &str) -> std::result::Result<S3Uri, String> {
    S3Uri::parse(text).map_err(|err| err.to_string())
}

/// Parses a `cp`/`mv` location: anything that looks like a URL (`s3:...`,
/// `x://...`) must be a valid S3 URI; everything else is a local path.
pub fn parse_location(text: &str) -> std::result::Result<Location, String> {
    let looks_like_url = text
        .get(..3)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("s3:"))
        || text.contains("://");
    if looks_like_url {
        return parse_s3_uri(text).map(Location::S3);
    }
    if text.is_empty() {
        return Err("the path is empty".to_owned());
    }
    Ok(Location::Local(PathBuf::from(text)))
}

/// Parses a bucket argument: `name`, `s3://name` or `s3://name/`.
pub fn parse_bucket(text: &str) -> std::result::Result<String, String> {
    let uri = if text
        .get(..S3Uri::SCHEME.len())
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case(S3Uri::SCHEME))
    {
        parse_s3_uri(text)?
    } else {
        S3Uri::new(text, "").map_err(|err| err.to_string())?
    };
    if !uri.is_bucket_root() {
        return Err(format!(
            "'{text}' names an object; expected a bucket such as s3://bucket"
        ));
    }
    Ok(uri.bucket().to_owned())
}

/// Destination of a single-object copy: a destination that is a bucket or
/// ends with `/` receives the source's name (`s3://b/photos/a.jpg` to
/// `s3://c/backup/` gives `s3://c/backup/a.jpg`); any other destination is
/// the exact target key.
pub fn single_destination(src: &S3Uri, dst: &S3Uri) -> Result<S3Uri> {
    if dst.is_prefix() {
        let name = strip_parent(src.key(), src.key())?;
        S3Uri::new(dst.bucket(), join_key(dst.key(), name))
    } else {
        Ok(dst.clone())
    }
}

/// The key relative to the source prefix, which `--include`/`--exclude`
/// patterns are matched against.
pub fn relative_to_source<'k>(src: &S3Uri, key: &'k str) -> &'k str {
    key.strip_prefix(src.key()).unwrap_or(key)
}

/// The prefix of a recursive upload: the destination key with exactly one
/// trailing `/` (or empty for a bucket).
pub fn upload_prefix(dst: &S3Uri) -> String {
    if dst.key().is_empty() || dst.key().ends_with('/') {
        dst.key().to_owned()
    } else {
        format!("{}/", dst.key())
    }
}

/// Rejects recursive S3 copies that would copy every object onto itself, or
/// write into the prefix being listed (which would never finish).
pub fn check_recursive_pair(src: &S3Uri, dst: &S3Uri) -> Result<()> {
    if src.bucket() != dst.bucket() {
        return Ok(());
    }
    let dst_prefix = upload_prefix(dst);
    if dst_prefix == parent_prefix(src.key()) {
        return Err(Error::Usage(format!(
            "copying {src} to {dst} would put every object onto itself"
        )));
    }
    if dst_prefix.starts_with(src.key()) {
        return Err(Error::Usage(format!(
            "the destination {dst} is inside the source {src}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(text: &str) -> S3Uri {
        S3Uri::parse(text).unwrap()
    }

    #[test]
    fn locations() {
        for text in [
            "file.txt",
            "./dir/",
            "/tmp/x",
            "C:\\data",
            "~/x",
            "s3bucket/key",
        ] {
            assert_eq!(
                parse_location(text).unwrap(),
                Location::Local(PathBuf::from(text)),
                "{text}"
            );
        }
        assert_eq!(
            parse_location("s3://b/k").unwrap(),
            Location::S3(uri("s3://b/k"))
        );
        assert_eq!(
            parse_location("S3://b/k").unwrap(),
            Location::S3(uri("s3://b/k"))
        );
        for text in ["s3:/bucket", "S3:bucket", "https://example.com/b"] {
            let err = parse_location(text).unwrap_err();
            assert!(err.contains("not an S3 URI"), "{text}: {err}");
        }
        assert!(parse_location("").is_err());
        assert_eq!(Location::S3(uri("s3://b/k")).to_string(), "s3://b/k");
        assert_eq!(Location::Local(PathBuf::from("a/b")).to_string(), "a/b");
    }

    #[test]
    fn bucket_arguments() {
        assert_eq!(parse_bucket("photos").unwrap(), "photos");
        assert_eq!(parse_bucket("s3://photos").unwrap(), "photos");
        assert_eq!(parse_bucket("s3://photos/").unwrap(), "photos");
        assert!(
            parse_bucket("s3://photos/a.jpg")
                .unwrap_err()
                .contains("names an object")
        );
        assert!(parse_bucket("").is_err());
        assert!(parse_bucket("bad bucket").is_err());
        assert!(parse_bucket("a/b").is_err());
    }

    #[test]
    fn single_object_destinations() {
        let src = uri("s3://b/photos/2024/a.jpg");
        assert_eq!(
            single_destination(&src, &uri("s3://c/backup/")).unwrap(),
            uri("s3://c/backup/a.jpg")
        );
        assert_eq!(
            single_destination(&src, &uri("s3://c")).unwrap(),
            uri("s3://c/a.jpg")
        );
        assert_eq!(
            single_destination(&src, &uri("s3://c/renamed.jpg")).unwrap(),
            uri("s3://c/renamed.jpg")
        );
    }

    #[test]
    fn relative_keys_and_upload_prefixes() {
        let src = uri("s3://b/photos/");
        assert_eq!(relative_to_source(&src, "photos/2024/a.jpg"), "2024/a.jpg");
        assert_eq!(relative_to_source(&src, "photos/"), "");
        assert_eq!(upload_prefix(&uri("s3://b")), "");
        assert_eq!(upload_prefix(&uri("s3://b/backup")), "backup/");
        assert_eq!(upload_prefix(&uri("s3://b/backup/")), "backup/");
    }

    #[test]
    fn overlapping_recursive_copies_are_rejected() {
        let check = |src: &str, dst: &str| check_recursive_pair(&uri(src), &uri(dst));
        assert!(check("s3://b/photos/", "s3://c/photos/").is_ok());
        assert!(check("s3://b/photos/", "s3://b/backup/").is_ok());
        assert!(check("s3://b/photos/", "s3://b/photos-old/").is_ok());
        assert!(
            check("s3://b/photos/2024/", "s3://b/photos/")
                .unwrap_err()
                .to_string()
                .contains("onto itself")
        );
        assert!(
            check("s3://b/", "s3://b/")
                .unwrap_err()
                .to_string()
                .contains("onto itself")
        );
        assert!(
            check("s3://b/photos/", "s3://b/photos/backup/")
                .unwrap_err()
                .to_string()
                .contains("inside")
        );
        assert!(
            check("s3://b/", "s3://b/copy/")
                .unwrap_err()
                .to_string()
                .contains("inside")
        );
    }
}
