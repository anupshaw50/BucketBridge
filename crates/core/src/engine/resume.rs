//! Resuming a multipart upload that has no saved state.
//!
//! An upload that was started by another run, another tool or an earlier
//! installation (it shows up in the multipart uploads tool) keeps the parts
//! it uploaded on the server. [`TransferEngine::check_upload_resume`] takes
//! the local file the user says it was made from and checks that the file
//! can finish it:
//!
//! * the part size is read from the parts on the server (every part but the
//!   last has the same size) and must split the file into the same parts;
//! * every uploaded part must be the bytes of the file at its position: by
//!   its checksum when the part has one, else by its ETag, which for a part
//!   that is not encrypted with a KMS key or a customer key is the MD5 of
//!   the part (`skip_part_verification` leaves the content check out for
//!   uploads where that does not hold).
//!
//! A file that does not match is refused with the reason; nothing on the
//! server is changed. A file that matches gives an [`UploadState`] with the
//! verified parts, which the transfer queue runs like any resumed upload:
//! only the missing parts are sent and the upload is completed.

use std::path::Path;
use std::sync::Arc;

use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};

use super::checksum::Checksummer;
use super::source::{FileSource, UploadSource};
use super::upload::{ListedPart, UploadState, UploadedPart, part_len, valid_plan};
use super::{TransferEngine, blocking, unquote};
use crate::error::{Error, Result};
use crate::ops::part_size_for;
use crate::settings::ChecksumAlgorithm;

/// Bytes read at a time while comparing a part.
const CHUNK: u64 = 1024 * 1024;

/// How an uploaded part was compared with the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartVerification {
    /// The checksum stored with the part equals the one of the file's bytes.
    Checksum,
    /// The ETag of the part equals the MD5 of the file's bytes.
    Etag,
    /// The content was not compared (`skip_part_verification`); the size
    /// and position were.
    Skipped,
    /// The part is not the bytes of the file.
    Mismatch,
}

/// A part that is already on the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumePart {
    /// Part number, from 1.
    pub number: u32,
    /// Size in bytes.
    pub size: u64,
    /// How it was compared with the file.
    pub verified: PartVerification,
}

/// Why a file cannot finish an upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeProblemCode {
    /// The file is smaller than the parts already uploaded.
    FileTooSmall,
    /// The sizes of the uploaded parts do not fit this file, or one part
    /// size cannot split it into at most 10,000 parts.
    PartSizes,
    /// An uploaded part differs from the file (see `part`).
    PartMismatch,
}

/// The reason a file is refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeProblem {
    /// Category.
    pub code: ResumeProblemCode,
    /// A sentence for the user.
    pub message: String,
    /// The first part that differs, for `part_mismatch`.
    pub part: Option<u32>,
}

/// What [`TransferEngine::check_upload_resume`] found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadResumeCheck {
    /// Bucket of the upload.
    pub bucket: String,
    /// Key of the upload.
    pub key: String,
    /// The upload.
    pub upload_id: String,
    /// Size of the chosen file.
    pub file_size: u64,
    /// Size of every part but the last.
    pub part_size: u64,
    /// Parts of the whole file.
    pub part_count: u32,
    /// The checksum algorithm the upload was created with (`none` for none).
    pub checksum_algorithm: ChecksumAlgorithm,
    /// The parts on the server, by number.
    pub parts: Vec<ResumePart>,
    /// Parts that still have to be uploaded.
    pub missing_parts: u32,
    /// Bytes already on the server.
    pub bytes_uploaded: u64,
    /// Bytes that still have to be uploaded.
    pub bytes_remaining: u64,
    /// Why the file is refused; `None` when it can finish the upload.
    pub problem: Option<ResumeProblem>,
}

impl UploadResumeCheck {
    /// Whether the file can finish the upload.
    pub fn ok(&self) -> bool {
        self.problem.is_none()
    }
}

impl TransferEngine {
    /// Checks whether the local file `path` can finish the unfinished
    /// multipart upload `upload_id` of `bucket`/`key` (see the
    /// [module documentation](self)). The state to start the upload with is
    /// returned when it can.
    ///
    /// A missing upload is `not_found`; a file that does not match is a
    /// [`UploadResumeCheck::problem`], not an error.
    pub async fn check_upload_resume(
        &self,
        path: &Path,
        bucket: &str,
        key: &str,
        upload_id: &str,
        skip_part_verification: bool,
    ) -> Result<(UploadResumeCheck, Option<UploadState>)> {
        crate::s3uri::validate_bucket_name(bucket)?;
        crate::s3uri::validate_key(key)?;
        if upload_id.is_empty() {
            return Err(Error::Usage("the upload ID is empty".into()));
        }
        let owned = path.to_path_buf();
        let source: Arc<dyn UploadSource> =
            Arc::new(blocking(move || FileSource::open(&owned)).await??);
        let size = source.size();
        let (algorithm, listed) = self
            .list_parts_detecting(bucket, key, upload_id, None)
            .await?;

        let mut check = UploadResumeCheck {
            bucket: bucket.to_owned(),
            key: key.to_owned(),
            upload_id: upload_id.to_owned(),
            file_size: size,
            part_size: 0,
            part_count: 0,
            checksum_algorithm: algorithm,
            parts: Vec::new(),
            missing_parts: 0,
            bytes_uploaded: listed.values().map(|p| p.size).sum(),
            bytes_remaining: 0,
            problem: None,
        };
        let refuse = |mut check: UploadResumeCheck, code, message: String, part| {
            check.problem = Some(ResumeProblem {
                code,
                message,
                part,
            });
            Ok((check, None))
        };

        if check.bytes_uploaded > size {
            return refuse(
                check,
                ResumeProblemCode::FileTooSmall,
                format!(
                    "the file has {size} bytes, but {} bytes were already uploaded",
                    listed.values().map(|p| p.size).sum::<u64>()
                ),
                None,
            );
        }
        let Some(part_size) = self.plan_part_size(size, &listed)? else {
            return refuse(
                check,
                ResumeProblemCode::PartSizes,
                "the sizes of the uploaded parts do not fit this file".into(),
                None,
            );
        };
        check.part_size = part_size;
        check.part_count = u32::try_from(size.div_ceil(part_size)).unwrap_or(u32::MAX);
        check.missing_parts = check
            .part_count
            .saturating_sub(u32::try_from(listed.len()).unwrap_or(u32::MAX));
        check.bytes_remaining = size.saturating_sub(check.bytes_uploaded);

        // Compare every uploaded part with the file.
        let mut state_parts = Vec::with_capacity(listed.len());
        for (number, part) in &listed {
            let verified = if skip_part_verification {
                PartVerification::Skipped
            } else {
                compare_part(
                    Arc::clone(&source),
                    (u64::from(*number) - 1) * part_size,
                    part,
                    algorithm,
                )
                .await?
            };
            check.parts.push(ResumePart {
                number: *number,
                size: part.size,
                verified,
            });
            if verified == PartVerification::Mismatch {
                let message = if part.checksum.is_some() {
                    format!(
                        "part {number} of the upload is not the same as the file (the checksums differ)"
                    )
                } else {
                    format!(
                        "part {number} of the upload is not the same as the file (the ETags differ)"
                    )
                };
                return refuse(
                    check,
                    ResumeProblemCode::PartMismatch,
                    message,
                    Some(*number),
                );
            }
            state_parts.push(UploadedPart {
                number: *number,
                size: part.size,
                etag: part.etag.clone(),
                checksum: part.checksum.clone(),
            });
        }
        let state = UploadState {
            bucket: bucket.to_owned(),
            key: key.to_owned(),
            upload_id: upload_id.to_owned(),
            source: source.fingerprint(),
            part_size,
            checksum_algorithm: algorithm,
            parts: state_parts,
        };
        Ok((check, Some(state)))
    }

    /// The part size that explains the parts on the server for a file of
    /// `size` bytes; with no parts yet, the configured one. `None` when no
    /// size fits.
    fn plan_part_size(
        &self,
        size: u64,
        listed: &std::collections::BTreeMap<u32, ListedPart>,
    ) -> Result<Option<u64>> {
        let fits = |part_size: u64| {
            valid_plan(size, part_size)
                && listed
                    .iter()
                    .all(|(number, part)| part_len(size, part_size, *number) == Some(part.size))
        };
        if listed.is_empty() {
            return part_size_for(size, self.options.part_size).map(Some);
        }
        // Every part but the last has the part size, so the first part (or,
        // without it, any part) says what it is.
        let mut candidates: Vec<u64> = listed.values().map(|p| p.size).collect();
        candidates.dedup();
        Ok(candidates.into_iter().find(|candidate| fits(*candidate)))
    }
}

/// Compares one uploaded part with the file at `offset`.
async fn compare_part(
    source: Arc<dyn UploadSource>,
    offset: u64,
    part: &ListedPart,
    algorithm: ChecksumAlgorithm,
) -> Result<PartVerification> {
    let len = part.size;
    let expected_checksum = part
        .checksum
        .clone()
        .filter(|_| algorithm != ChecksumAlgorithm::None);
    let etag = unquote(&part.etag).to_ascii_lowercase();
    let description = source.describe();
    let done = blocking(move || {
        let mut checksummer = expected_checksum
            .as_ref()
            .and_then(|_| Checksummer::new(algorithm));
        let mut md5 = checksummer.is_none().then(Md5::new);
        let capacity = usize::try_from(len.min(CHUNK)).unwrap_or(1 << 20);
        let mut buf = vec![0u8; capacity];
        let mut done = 0u64;
        while done < len {
            let step = usize::try_from((len - done).min(CHUNK)).unwrap_or(capacity);
            let chunk = &mut buf[..step];
            source.read_exact_at(offset + done, chunk)?;
            if let Some(checksummer) = checksummer.as_mut() {
                checksummer.update(chunk);
            }
            if let Some(md5) = md5.as_mut() {
                md5.update(&*chunk);
            }
            done += step as u64;
        }
        Ok::<_, std::io::Error>(match (checksummer, md5, expected_checksum) {
            (Some(checksummer), _, Some(expected)) => {
                if checksummer.finish() == expected {
                    PartVerification::Checksum
                } else {
                    PartVerification::Mismatch
                }
            }
            (_, Some(md5), _) => {
                if hex::encode(md5.finalize()) == etag {
                    PartVerification::Etag
                } else {
                    PartVerification::Mismatch
                }
            }
            _ => PartVerification::Mismatch,
        })
    })
    .await?;
    done.map_err(|err| Error::io(format!("reading {description}"), err))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_and_problem_codes_use_snake_case() {
        assert_eq!(
            serde_json::to_value(PartVerification::Etag).unwrap(),
            serde_json::json!("etag")
        );
        assert_eq!(
            serde_json::to_value(ResumeProblemCode::PartMismatch).unwrap(),
            serde_json::json!("part_mismatch")
        );
    }
}
