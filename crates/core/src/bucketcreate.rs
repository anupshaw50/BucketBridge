//! The advanced options of creating a bucket ([`S3::create_bucket_with`]).
//!
//! [`S3::create_bucket_with`]: crate::S3::create_bucket_with

use serde::{Deserialize, Serialize};

use crate::bucketcfg::PublicAccessBlock;
use crate::objectlock::ObjectOwnership;

/// What a new bucket gets besides its region. The default asks for nothing
/// beyond what the server does on its own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateBucketOptions {
    /// Turn Object Lock on (S3 turns versioning on with it). It can never be
    /// switched off again.
    #[serde(default)]
    pub object_lock: bool,
    /// The Object Ownership setting; `None` leaves it to the server (AWS:
    /// BucketOwnerEnforced).
    #[serde(default)]
    pub ownership: Option<ObjectOwnership>,
    /// The public access block to apply once the bucket exists; `None` leaves
    /// it to the server.
    #[serde(default)]
    pub public_access_block: Option<PublicAccessBlock>,
}

/// The outcome of creating a bucket.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateBucketResult {
    /// What could not be applied although the bucket exists, for example a
    /// public access block the server does not implement.
    pub warnings: Vec<String>,
}
