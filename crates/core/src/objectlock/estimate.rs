//! Cost estimate for a restore, per retrieval tier.
//!
//! The prices are a bundled table of AWS list prices for us-east-1 (see
//! [`PRICE_DATE`]); nothing is fetched from the network. They are
//! approximate: other regions, discounts, taxes and the temporary copy kept
//! for the restore days are not included, so the figure is a guide, not a
//! bill. [`estimate_restore`] is a pure function of a [`RestoreSummary`].

use futures::StreamExt;
use serde::{Deserialize, Serialize};

use super::{ArchiveKind, RestoreTier};
use crate::error::Result;
use crate::objectmeta::{archive_kind, unrestored_archive};
use crate::ops::S3;
use crate::s3uri::{validate_bucket_name, validate_key};

/// The date of the bundled price table.
pub const PRICE_DATE: &str = "2025-06-01";

/// Text that goes with every estimate.
pub const PRICE_NOTE: &str = "Approximate, us-east-1 list prices as of 2025-06-01; check your AWS bill. Taxes, discounts, other regions and the storage of the restored copy are not included.";

/// Most objects whose size is read for one estimate.
pub const ESTIMATE_MAX_OBJECTS: usize = 1000;

/// Bytes in the "GB" AWS bills (GiB).
const GB: f64 = 1_073_741_824.0;

/// What a tier costs for one kind of archive, in US dollars.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TierPrice {
    /// Per GB retrieved.
    pub per_gb: f64,
    /// Per 1,000 restore requests.
    pub per_1000_requests: f64,
}

/// The list price of `tier` for `kind`, or `None` when that tier cannot
/// restore that kind of archive.
pub fn tier_price(kind: ArchiveKind, tier: RestoreTier) -> Option<TierPrice> {
    let price = |per_gb, per_1000_requests| {
        Some(TierPrice {
            per_gb,
            per_1000_requests,
        })
    };
    match (kind, tier) {
        (ArchiveKind::Glacier, RestoreTier::Expedited) => price(0.03, 10.0),
        (ArchiveKind::Glacier, RestoreTier::Standard) => price(0.01, 0.05),
        (ArchiveKind::Glacier, RestoreTier::Bulk) => price(0.0, 0.025),
        (ArchiveKind::DeepArchive, RestoreTier::Standard) => price(0.02, 0.10),
        (ArchiveKind::DeepArchive, RestoreTier::Bulk) => price(0.0025, 0.025),
        // Intelligent-Tiering has no retrieval or restore request charge.
        (
            ArchiveKind::IntelligentArchive | ArchiveKind::IntelligentDeepArchive,
            RestoreTier::Standard | RestoreTier::Bulk,
        ) => price(0.0, 0.0),
        _ => None,
    }
}

/// Archived objects of one kind in a selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveGroup {
    /// The archive they sit in.
    pub kind: ArchiveKind,
    /// How many objects.
    pub objects: u64,
    /// Their total size in bytes.
    pub bytes: u64,
}

/// What a selection holds, as far as a restore is concerned.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreSummary {
    /// Archived objects, one group per kind that occurs.
    pub groups: Vec<ArchiveGroup>,
    /// Objects that are not archived (nothing to restore).
    pub not_archived: u64,
    /// Objects whose state could not be read.
    pub unreadable: u64,
    /// Only the first [`ESTIMATE_MAX_OBJECTS`] objects were read.
    pub truncated: bool,
}

impl RestoreSummary {
    /// Archived objects in all groups.
    pub fn objects(&self) -> u64 {
        self.groups.iter().map(|group| group.objects).sum()
    }

    /// Their total size in bytes.
    pub fn bytes(&self) -> u64 {
        self.groups.iter().map(|group| group.bytes).sum()
    }

    /// Counts one archived object.
    pub fn add(&mut self, kind: ArchiveKind, bytes: u64) {
        if let Some(group) = self.groups.iter_mut().find(|group| group.kind == kind) {
            group.objects += 1;
            group.bytes += bytes;
        } else {
            self.groups.push(ArchiveGroup {
                kind,
                objects: 1,
                bytes,
            });
        }
    }
}

/// The estimate for one tier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TierEstimate {
    /// The tier.
    pub tier: RestoreTier,
    /// Retrieval plus requests, in US dollars.
    pub total_usd: f64,
    /// The per GB part.
    pub retrieval_usd: f64,
    /// The per request part.
    pub requests_usd: f64,
    /// Objects this tier can restore.
    pub objects: u64,
    /// Archived objects this tier cannot restore (it is skipped for them).
    pub unsupported: u64,
}

/// The estimate for a selection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestoreEstimate {
    /// What the selection holds.
    pub summary: RestoreSummary,
    /// Archived objects.
    pub objects: u64,
    /// Their total size in bytes.
    pub bytes: u64,
    /// One entry per tier: Expedited, Standard, Bulk.
    pub tiers: Vec<TierEstimate>,
    /// The date of the price table.
    pub price_date: String,
    /// The disclaimer to show next to the figures.
    pub note: String,
}

/// Prices a restore of `summary` with each tier. A pure function of the
/// bundled price table.
pub fn estimate_restore(summary: &RestoreSummary) -> RestoreEstimate {
    let tiers = [
        RestoreTier::Expedited,
        RestoreTier::Standard,
        RestoreTier::Bulk,
    ]
    .into_iter()
    .map(|tier| {
        let (mut retrieval, mut requests, mut objects, mut unsupported) = (0.0, 0.0, 0, 0);
        for group in &summary.groups {
            match tier_price(group.kind, tier) {
                Some(price) => {
                    retrieval += group.bytes as f64 / GB * price.per_gb;
                    requests += group.objects as f64 / 1000.0 * price.per_1000_requests;
                    objects += group.objects;
                }
                None => unsupported += group.objects,
            }
        }
        TierEstimate {
            tier,
            total_usd: retrieval + requests,
            retrieval_usd: retrieval,
            requests_usd: requests,
            objects,
            unsupported,
        }
    })
    .collect();
    RestoreEstimate {
        objects: summary.objects(),
        bytes: summary.bytes(),
        summary: summary.clone(),
        tiers,
        price_date: PRICE_DATE.to_owned(),
        note: PRICE_NOTE.to_owned(),
    }
}

impl S3 {
    /// Reads the archive state and size of `keys` (at most
    /// [`ESTIMATE_MAX_OBJECTS`], the rest are left out and `truncated` is set)
    /// with `HeadObject`, and counts them per archive kind. Objects with a
    /// restore that is running or finished are counted as not needing one.
    pub async fn restore_summary(&self, bucket: &str, keys: &[String]) -> Result<RestoreSummary> {
        validate_bucket_name(bucket)?;
        for key in keys {
            validate_key(key)?;
        }
        let mut summary = RestoreSummary {
            truncated: keys.len() > ESTIMATE_MAX_OBJECTS,
            ..RestoreSummary::default()
        };
        let client = self.client_for(bucket).await?;
        let files: Vec<String> = keys
            .iter()
            .filter(|key| !key.ends_with('/'))
            .take(ESTIMATE_MAX_OBJECTS)
            .cloned()
            .collect();
        let mut heads = futures::stream::iter(files)
            .map(|key| {
                let client = client.clone();
                async move {
                    client
                        .head_object()
                        .bucket(bucket)
                        .key(key)
                        .set_request_payer(self.request_payer())
                        .customize()
                        .config_override(self.metadata_timeout())
                        .send()
                        .await
                }
            })
            .buffered(16);
        while let Some(head) = heads.next().await {
            match head {
                Ok(head) => match archive_kind(&head) {
                    Some(kind) if unrestored_archive(&head).is_some() => {
                        let size = u64::try_from(head.content_length().unwrap_or(0)).unwrap_or(0);
                        summary.add(kind, size);
                    }
                    _ => summary.not_archived += 1,
                },
                Err(_) => summary.unreadable += 1,
            }
        }
        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn summary(groups: &[(ArchiveKind, u64, u64)]) -> RestoreSummary {
        let mut summary = RestoreSummary::default();
        for &(kind, objects, bytes) in groups {
            summary.groups.push(ArchiveGroup {
                kind,
                objects,
                bytes,
            });
        }
        summary
    }

    fn tier(estimate: &RestoreEstimate, tier: RestoreTier) -> &TierEstimate {
        estimate.tiers.iter().find(|t| t.tier == tier).unwrap()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn glacier_flexible_is_priced_per_gb_and_per_request() {
        let estimate = estimate_restore(&summary(&[(ArchiveKind::Glacier, 2000, 100 * GIB)]));
        assert_eq!((estimate.objects, estimate.bytes), (2000, 100 * GIB));
        let expedited = tier(&estimate, RestoreTier::Expedited);
        assert!(close(expedited.retrieval_usd, 3.0));
        assert!(close(expedited.requests_usd, 20.0));
        assert!(close(expedited.total_usd, 23.0));
        let standard = tier(&estimate, RestoreTier::Standard);
        assert!(close(standard.total_usd, 1.0 + 0.1));
        let bulk = tier(&estimate, RestoreTier::Bulk);
        assert!(close(bulk.retrieval_usd, 0.0));
        assert!(close(bulk.total_usd, 0.05));
    }

    #[test]
    fn deep_archive_has_no_expedited_tier() {
        let estimate = estimate_restore(&summary(&[(ArchiveKind::DeepArchive, 1000, 10 * GIB)]));
        let expedited = tier(&estimate, RestoreTier::Expedited);
        assert_eq!((expedited.objects, expedited.unsupported), (0, 1000));
        assert!(close(expedited.total_usd, 0.0));
        let standard = tier(&estimate, RestoreTier::Standard);
        assert!(close(standard.total_usd, 0.2 + 0.1));
        let bulk = tier(&estimate, RestoreTier::Bulk);
        assert!(close(bulk.total_usd, 0.025 + 0.025));
    }

    #[test]
    fn a_mixed_selection_adds_the_kinds_and_counts_the_unsupported() {
        let estimate = estimate_restore(&summary(&[
            (ArchiveKind::Glacier, 1000, GIB),
            (ArchiveKind::DeepArchive, 1000, GIB),
            (ArchiveKind::IntelligentArchive, 5, 50 * GIB),
        ]));
        assert_eq!(estimate.objects, 2005);
        let expedited = tier(&estimate, RestoreTier::Expedited);
        assert_eq!((expedited.objects, expedited.unsupported), (1000, 1005));
        let standard = tier(&estimate, RestoreTier::Standard);
        assert_eq!(standard.unsupported, 0);
        assert!(close(standard.total_usd, (0.01 + 0.05) + (0.02 + 0.10)));
    }

    #[test]
    fn intelligent_tiering_archives_cost_nothing_to_restore() {
        let estimate = estimate_restore(&summary(&[
            (ArchiveKind::IntelligentArchive, 10, GIB),
            (ArchiveKind::IntelligentDeepArchive, 10, GIB),
        ]));
        assert!(close(tier(&estimate, RestoreTier::Standard).total_usd, 0.0));
        assert!(close(tier(&estimate, RestoreTier::Bulk).total_usd, 0.0));
        assert_eq!(tier(&estimate, RestoreTier::Expedited).unsupported, 20);
    }

    #[test]
    fn an_empty_selection_costs_nothing_and_carries_the_date() {
        let estimate = estimate_restore(&RestoreSummary::default());
        assert!(estimate.tiers.iter().all(|t| close(t.total_usd, 0.0)));
        assert_eq!(estimate.tiers.len(), 3);
        assert_eq!(estimate.price_date, PRICE_DATE);
        assert!(estimate.note.contains(PRICE_DATE));
        assert!(estimate.note.contains("check your AWS bill"));
    }

    #[test]
    fn the_summary_adds_per_kind() {
        let mut summary = RestoreSummary::default();
        summary.add(ArchiveKind::Glacier, 5);
        summary.add(ArchiveKind::Glacier, 7);
        summary.add(ArchiveKind::DeepArchive, 1);
        assert_eq!((summary.objects(), summary.bytes()), (3, 13));
        assert_eq!(summary.groups.len(), 2);
    }
}
