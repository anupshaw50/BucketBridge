//! Multipart planning: single request or multipart, part size and part ranges.
//!
//! Rules (sizes in bytes):
//!
//! * An object is transferred in parts when `size >= threshold`, and always
//!   when it is larger than [`MAX_PART_SIZE`], because a single request has the
//!   same 5 GiB limit as one part. An empty object is always a single request
//!   (a multipart upload needs at least one part).
//! * The part size is the configured size, raised to at least
//!   [`MIN_PART_SIZE`] and lowered to at most [`MAX_PART_SIZE`].
//! * If that would need more than [`MAX_PART_COUNT`] parts, the part size grows
//!   to `ceil(size / 10,000)` rounded up to a whole MiB.
//! * Sizes above [`MAX_MULTIPART_SIZE`] (10,000 parts of 5 GiB) are rejected
//!   with a usage error. No provider maximum object size is assumed beyond
//!   that.
//!
//! Part numbers start at 1. Every part except the last has exactly
//! `part_size` bytes; the last one has the rest (at least one byte).

use std::iter::FusedIterator;
use std::ops::Range;

use super::new_error;
use crate::error::{ErrorKind, Result};
use crate::settings::TransferSettings;

/// One mebibyte.
pub const MIB: u64 = 1024 * 1024;
/// Smallest part size (except for the last part).
pub const MIN_PART_SIZE: u64 = 5 * MIB;
/// Largest part size, and the largest single request.
pub const MAX_PART_SIZE: u64 = 5 * 1024 * MIB;
/// Most parts in one multipart transfer.
pub const MAX_PART_COUNT: u32 = 10_000;
/// Largest object the part rules can describe: 10,000 parts of 5 GiB.
pub const MAX_MULTIPART_SIZE: u64 = MAX_PART_SIZE * MAX_PART_COUNT as u64;

/// How an object of a given size is transferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferPlan {
    /// One request carries the whole object.
    Single,
    /// The object is split into parts.
    Multipart(MultipartPlan),
}

impl TransferPlan {
    /// Whether the object is transferred in parts.
    pub fn is_multipart(&self) -> bool {
        matches!(self, Self::Multipart(_))
    }
}

/// Part layout of a multipart transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MultipartPlan {
    object_size: u64,
    part_size: u64,
    part_count: u32,
}

impl MultipartPlan {
    /// Size of the whole object in bytes.
    pub fn object_size(&self) -> u64 {
        self.object_size
    }

    /// Size of every part but the last.
    pub fn part_size(&self) -> u64 {
        self.part_size
    }

    /// Number of parts (1 to 10,000).
    pub fn part_count(&self) -> u32 {
        self.part_count
    }

    /// Part `number` (1-based), or `None` when out of range.
    pub fn part(&self, number: u32) -> Option<Part> {
        if number == 0 || number > self.part_count {
            return None;
        }
        let start = u64::from(number - 1) * self.part_size;
        let end = start.saturating_add(self.part_size).min(self.object_size);
        Some(Part {
            number,
            range: start..end,
        })
    }

    /// All parts in order.
    pub fn parts(&self) -> Parts {
        Parts {
            plan: *self,
            front: 1,
            back: self.part_count,
        }
    }
}

/// One part: its 1-based number and its half-open byte range in the object.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Part {
    /// Part number, from 1 to the plan's part count.
    pub number: u32,
    /// Byte range of the part in the object (`start..end`, end exclusive).
    pub range: Range<u64>,
}

impl Part {
    /// Offset of the first byte.
    pub fn offset(&self) -> u64 {
        self.range.start
    }

    /// Number of bytes in the part (never 0).
    pub fn size(&self) -> u64 {
        self.range.end - self.range.start
    }

    /// The HTTP `Range` header value for this part (`bytes=first-last`).
    pub fn http_range(&self) -> String {
        format!(
            "bytes={}-{}",
            self.range.start,
            self.range.end.saturating_sub(1)
        )
    }
}

/// Iterator over the parts of a [`MultipartPlan`].
#[derive(Debug, Clone)]
pub struct Parts {
    plan: MultipartPlan,
    /// Next part number from the front.
    front: u32,
    /// Next part number from the back; empty once `front > back`.
    back: u32,
}

impl Parts {
    fn remaining(&self) -> usize {
        if self.front > self.back {
            0
        } else {
            (self.back - self.front + 1) as usize
        }
    }
}

impl Iterator for Parts {
    type Item = Part;

    fn next(&mut self) -> Option<Part> {
        if self.front > self.back {
            return None;
        }
        let part = self.plan.part(self.front);
        self.front += 1;
        part
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.remaining();
        (remaining, Some(remaining))
    }
}

impl DoubleEndedIterator for Parts {
    fn next_back(&mut self) -> Option<Part> {
        if self.front > self.back {
            return None;
        }
        let part = self.plan.part(self.back);
        // `back` is at least `front` >= 1 here, so this cannot underflow.
        self.back -= 1;
        part
    }
}

impl ExactSizeIterator for Parts {}

impl FusedIterator for Parts {}

/// Plans the transfer of `size` bytes with a multipart `threshold` and a
/// configured `part_size` (both in bytes). See the module docs for the rules.
pub fn plan_transfer(size: u64, threshold: u64, part_size: u64) -> Result<TransferPlan> {
    if size > MAX_MULTIPART_SIZE {
        return Err(new_error(
            ErrorKind::Usage,
            format!(
                "an object of {size} bytes is too large: at most {MAX_PART_COUNT} parts of \
                 {MAX_PART_SIZE} bytes ({MAX_MULTIPART_SIZE} bytes) can be transferred"
            ),
        ));
    }
    if size == 0 || (size < threshold && size <= MAX_PART_SIZE) {
        return Ok(TransferPlan::Single);
    }
    let mut part_size = part_size.clamp(MIN_PART_SIZE, MAX_PART_SIZE);
    let max_parts = u64::from(MAX_PART_COUNT);
    if size.div_ceil(part_size) > max_parts {
        // ceil(size / 10,000) <= 5 GiB because size <= MAX_MULTIPART_SIZE, and
        // 5 GiB is a whole number of MiB, so rounding up stays within limits.
        part_size = size.div_ceil(max_parts).div_ceil(MIB) * MIB;
    }
    let part_count = u32::try_from(size.div_ceil(part_size))
        .ok()
        .filter(|count| *count <= MAX_PART_COUNT)
        .ok_or_else(|| {
            new_error(
                ErrorKind::Internal,
                format!("part planning for {size} bytes exceeded {MAX_PART_COUNT} parts"),
            )
        })?;
    Ok(TransferPlan::Multipart(MultipartPlan {
        object_size: size,
        part_size,
        part_count,
    }))
}

/// [`plan_transfer`] with the threshold and part size from the settings
/// (configured in MiB).
pub fn plan_for_settings(size: u64, settings: &TransferSettings) -> Result<TransferPlan> {
    plan_transfer(
        size,
        settings.multipart_threshold_mib.saturating_mul(MIB),
        settings.part_size_mib.saturating_mul(MIB),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * MIB;
    const DEFAULT_THRESHOLD: u64 = 8 * MIB;
    const DEFAULT_PART: u64 = 8 * MIB;

    fn multipart(size: u64, threshold: u64, part: u64) -> MultipartPlan {
        match plan_transfer(size, threshold, part).unwrap() {
            TransferPlan::Multipart(plan) => plan,
            TransferPlan::Single => panic!("expected multipart for {size} bytes"),
        }
    }

    fn is_single(size: u64, threshold: u64, part: u64) -> bool {
        plan_transfer(size, threshold, part).unwrap() == TransferPlan::Single
    }

    /// Checks every invariant of a plan, walking all of its parts.
    fn check_plan(plan: &MultipartPlan) {
        let size = plan.object_size();
        assert!(plan.part_count() >= 1 && plan.part_count() <= MAX_PART_COUNT);
        assert!(plan.part_size() >= MIN_PART_SIZE && plan.part_size() <= MAX_PART_SIZE);
        assert_eq!(
            u64::from(plan.part_count()),
            size.div_ceil(plan.part_size())
        );
        let mut next = 0;
        let mut count = 0;
        for (index, part) in plan.parts().enumerate() {
            assert_eq!(part.number as usize, index + 1);
            assert_eq!(part.offset(), next, "parts are contiguous");
            assert!(part.size() > 0);
            if part.number < plan.part_count() {
                assert_eq!(part.size(), plan.part_size());
            } else {
                assert!(part.size() <= plan.part_size());
            }
            next = part.range.end;
            count += 1;
        }
        assert_eq!(next, size, "parts cover the object");
        assert_eq!(count, plan.part_count());
    }

    #[test]
    fn empty_object_is_a_single_request() {
        assert!(is_single(0, DEFAULT_THRESHOLD, DEFAULT_PART));
        assert!(is_single(0, 0, DEFAULT_PART));
    }

    #[test]
    fn threshold_boundaries() {
        assert!(is_single(1, DEFAULT_THRESHOLD, DEFAULT_PART));
        assert!(is_single(
            DEFAULT_THRESHOLD - 1,
            DEFAULT_THRESHOLD,
            DEFAULT_PART
        ));
        let at = multipart(DEFAULT_THRESHOLD, DEFAULT_THRESHOLD, DEFAULT_PART);
        assert_eq!((at.part_size(), at.part_count()), (8 * MIB, 1));
        let above = multipart(DEFAULT_THRESHOLD + 1, DEFAULT_THRESHOLD, DEFAULT_PART);
        assert_eq!((above.part_size(), above.part_count()), (8 * MIB, 2));
        assert_eq!(above.part(2).unwrap().size(), 1);
        check_plan(&at);
        check_plan(&above);
    }

    #[test]
    fn threshold_zero_or_one_splits_any_non_empty_object() {
        let one_byte = multipart(1, 0, DEFAULT_PART);
        assert_eq!((one_byte.part_size(), one_byte.part_count()), (8 * MIB, 1));
        assert_eq!(one_byte.part(1).unwrap().range, 0..1);
        let small = multipart(2 * MIB, 1, DEFAULT_PART);
        assert_eq!(small.part_count(), 1);
        check_plan(&small);
    }

    #[test]
    fn huge_threshold_still_splits_objects_above_the_single_request_limit() {
        assert!(is_single(MAX_PART_SIZE, u64::MAX, DEFAULT_PART));
        let plan = multipart(MAX_PART_SIZE + 1, u64::MAX, DEFAULT_PART);
        assert_eq!(plan.part_size(), 8 * MIB);
        assert_eq!(plan.part_count(), 641);
        check_plan(&plan);
    }

    #[test]
    fn part_size_is_clamped_to_limits() {
        for configured in [0, 1, MIB, MIN_PART_SIZE - 1] {
            let plan = multipart(100 * MIB, 0, configured);
            assert_eq!(plan.part_size(), MIN_PART_SIZE, "configured {configured}");
            assert_eq!(plan.part_count(), 20);
            check_plan(&plan);
        }
        let exact = multipart(100 * MIB, 0, MIN_PART_SIZE);
        assert_eq!(exact.part_size(), MIN_PART_SIZE);
        for configured in [MAX_PART_SIZE + 1, 6 * GIB, u64::MAX] {
            let plan = multipart(12 * GIB, 0, configured);
            assert_eq!(plan.part_size(), MAX_PART_SIZE, "configured {configured}");
            assert_eq!(plan.part_count(), 3);
            check_plan(&plan);
        }
    }

    #[test]
    fn unaligned_part_size_is_kept_when_no_growth_is_needed() {
        let configured = 5 * MIB + 123;
        let plan = multipart(64 * MIB, 0, configured);
        assert_eq!(plan.part_size(), configured);
        assert_eq!(plan.part_count(), 13);
        check_plan(&plan);
    }

    #[test]
    fn part_count_limit_boundaries_at_the_configured_size() {
        // Exactly 10,000 parts of 8 MiB: no growth.
        let size = 10_000 * 8 * MIB;
        let plan = multipart(size, DEFAULT_THRESHOLD, DEFAULT_PART);
        assert_eq!((plan.part_size(), plan.part_count()), (8 * MIB, 10_000));
        check_plan(&plan);
        // One more byte: grow to ceil(size / 10,000) rounded up to 9 MiB.
        let plan = multipart(size + 1, DEFAULT_THRESHOLD, DEFAULT_PART);
        assert_eq!(plan.part_size(), 9 * MIB);
        assert_eq!(u64::from(plan.part_count()), (size + 1).div_ceil(9 * MIB));
        assert!(plan.part_count() <= MAX_PART_COUNT);
        check_plan(&plan);
        // 10,000 parts of 5 MiB with the minimum part size.
        let size = 10_000 * MIN_PART_SIZE;
        let plan = multipart(size, 0, 0);
        assert_eq!(
            (plan.part_size(), plan.part_count()),
            (MIN_PART_SIZE, 10_000)
        );
        let plan = multipart(size + 1, 0, 0);
        assert_eq!(plan.part_size(), 6 * MIB);
        check_plan(&plan);
    }

    #[test]
    fn grown_part_size_is_a_whole_mib_and_minimal() {
        for size in [
            100 * GIB,
            100 * GIB + 1,
            500 * GIB - 1,
            1024 * GIB,
            5 * 1024 * GIB,
            48_828 * GIB + 12_345,
        ] {
            let plan = multipart(size, DEFAULT_THRESHOLD, DEFAULT_PART);
            assert_eq!(plan.part_size() % MIB, 0, "size {size}");
            assert!(plan.part_count() <= MAX_PART_COUNT);
            // One MiB less would need more than 10,000 parts.
            assert!(size.div_ceil(plan.part_size() - MIB) > u64::from(MAX_PART_COUNT));
            check_plan(&plan);
        }
    }

    #[test]
    fn maximum_size_boundaries() {
        let plan = multipart(MAX_MULTIPART_SIZE, DEFAULT_THRESHOLD, DEFAULT_PART);
        assert_eq!(
            (plan.part_size(), plan.part_count()),
            (MAX_PART_SIZE, 10_000)
        );
        assert_eq!(plan.part(10_000).unwrap().size(), MAX_PART_SIZE);
        let plan = multipart(MAX_MULTIPART_SIZE - 1, DEFAULT_THRESHOLD, DEFAULT_PART);
        assert_eq!(
            (plan.part_size(), plan.part_count()),
            (MAX_PART_SIZE, 10_000)
        );
        assert_eq!(plan.part(10_000).unwrap().size(), MAX_PART_SIZE - 1);
        for size in [MAX_MULTIPART_SIZE + 1, u64::MAX] {
            let error = plan_transfer(size, DEFAULT_THRESHOLD, DEFAULT_PART).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Usage, "size {size}");
            assert!(error.to_string().contains("too large"));
        }
    }

    #[test]
    fn part_lookup_and_ranges() {
        let plan = multipart(20 * MIB + 5, 0, DEFAULT_PART);
        assert_eq!(plan.part_count(), 3);
        assert_eq!(plan.part(0), None);
        assert_eq!(plan.part(4), None);
        assert_eq!(plan.part(u32::MAX), None);
        let first = plan.part(1).unwrap();
        assert_eq!(first.range, 0..8 * MIB);
        assert_eq!(first.http_range(), format!("bytes=0-{}", 8 * MIB - 1));
        let last = plan.part(3).unwrap();
        assert_eq!(last.range, 16 * MIB..20 * MIB + 5);
        assert_eq!(last.size(), 4 * MIB + 5);
        assert_eq!(
            last.http_range(),
            format!("bytes={}-{}", 16 * MIB, 20 * MIB + 4)
        );
    }

    #[test]
    fn parts_iterator_is_exact_double_ended_and_fused() {
        let plan = multipart(40 * MIB, 0, DEFAULT_PART);
        let mut parts = plan.parts();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts.next().unwrap().number, 1);
        assert_eq!(parts.next_back().unwrap().number, 5);
        assert_eq!(parts.len(), 3);
        let middle: Vec<u32> = parts.by_ref().map(|part| part.number).collect();
        assert_eq!(middle, [2, 3, 4]);
        assert_eq!(parts.len(), 0);
        assert_eq!(parts.next(), None);
        assert_eq!(parts.next_back(), None);
        assert_eq!(parts.next(), None);
        let reversed: Vec<u32> = plan.parts().rev().map(|part| part.number).collect();
        assert_eq!(reversed, [5, 4, 3, 2, 1]);

        let single = multipart(1, 0, DEFAULT_PART);
        let mut parts = single.parts();
        assert_eq!(parts.next_back().unwrap().number, 1);
        assert_eq!(parts.next(), None);
    }

    #[test]
    fn settings_are_converted_from_mib() {
        let settings = TransferSettings::default();
        assert_eq!(
            plan_for_settings(8 * MIB - 1, &settings).unwrap(),
            TransferPlan::Single
        );
        let plan = plan_for_settings(8 * MIB, &settings).unwrap();
        assert!(plan.is_multipart());
        let huge = TransferSettings {
            multipart_threshold_mib: u64::MAX,
            part_size_mib: u64::MAX,
            ..TransferSettings::default()
        };
        assert_eq!(plan_for_settings(GIB, &huge).unwrap(), TransferPlan::Single);
        match plan_for_settings(6 * GIB, &huge).unwrap() {
            TransferPlan::Multipart(plan) => assert_eq!(plan.part_size(), MAX_PART_SIZE),
            TransferPlan::Single => panic!("6 GiB cannot be a single request"),
        }
    }

    #[test]
    fn sweep_of_sizes_keeps_all_invariants() {
        let sizes = [
            1,
            MIB,
            MIN_PART_SIZE - 1,
            MIN_PART_SIZE,
            MIN_PART_SIZE + 1,
            GIB,
            MAX_PART_SIZE - 1,
            MAX_PART_SIZE,
            MAX_PART_SIZE + 1,
            10 * GIB + 7,
            1024 * GIB - 1,
            MAX_MULTIPART_SIZE / 2 + 3,
        ];
        for size in sizes {
            for part in [0, MIN_PART_SIZE, DEFAULT_PART, 64 * MIB + 1, MAX_PART_SIZE] {
                check_plan(&multipart(size, 0, part));
            }
        }
    }

    /// The examples table in `docs/transfers.md`.
    #[test]
    fn documented_examples() {
        let defaults = TransferSettings::default();
        let plan = |size| plan_for_settings(size, &defaults).unwrap();
        let layout = |plan: TransferPlan| match plan {
            TransferPlan::Multipart(p) => Some((p.part_count(), p.part_size() / MIB)),
            TransferPlan::Single => None,
        };
        assert_eq!(layout(plan(MIB)), None);
        assert_eq!(layout(plan(8 * MIB)), Some((1, 8)));
        assert_eq!(layout(plan(100 * GIB)), Some((9_310, 11)));
        assert_eq!(layout(plan(1024 * GIB)), Some((9_987, 105)));
        let high_threshold = TransferSettings {
            multipart_threshold_mib: 10 * 1024,
            ..TransferSettings::default()
        };
        let six = plan_for_settings(6 * GIB, &high_threshold).unwrap();
        assert_eq!(layout(six), Some((768, 8)));
        let too_large = plan_for_settings(MAX_MULTIPART_SIZE + 1, &defaults).unwrap_err();
        assert_eq!(too_large.kind(), ErrorKind::Usage);
    }
}
