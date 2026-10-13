//! Byte-range allocation and management.
//!
//! # Invariant
//!
//! Byte ranges assigned to workers **never overlap** and **always cover the
//! file exactly**. This is enforced by construction and verified by property
//! tests.

use serde::{Deserialize, Serialize};

/// A contiguous byte range [start, end] (both inclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteRange {
    /// Start byte offset (inclusive).
    pub start: u64,
    /// End byte offset (inclusive).
    pub end: u64,
}

impl ByteRange {
    /// Creates a new byte range. Returns `None` if `start > end`.
    pub fn new(start: u64, end: u64) -> Option<Self> {
        if start > end || end - start == u64::MAX {
            None
        } else {
            Some(Self { start, end })
        }
    }

    /// Returns the length of this range in bytes.
    pub fn length(&self) -> u64 {
        self.end - self.start + 1
    }

    /// Returns true if this range overlaps with another.
    pub fn overlaps(&self, other: &ByteRange) -> bool {
        self.start <= other.end && other.start <= self.end
    }

    /// Returns true if this range is adjacent to (immediately before) another.
    pub fn is_adjacent_before(&self, other: &ByteRange) -> bool {
        self.end.checked_add(1) == Some(other.start)
    }
}

/// Allocates non-overlapping byte ranges for parallel download segments.
///
/// # Arguments
///
/// * `total_size` - Total file size in bytes.
/// * `max_segments` - Maximum number of segments to create.
/// * `min_segment_size` - Minimum bytes per segment.
/// * `max_segment_size` - Request quantum bound; logical ownership can be larger
///   so the segment count remains bounded for arbitrarily large files.
///
/// # Invariants guaranteed
///
/// 1. Ranges never overlap.
/// 2. Ranges cover [0, total_size - 1] exactly.
/// 3. Segment count ≤ `max_segments`.
/// 4. Each segment ≥ `min_segment_size` (except possibly the last if
///    `total_size < min_segment_size * max_segments`).
pub fn allocate_ranges(
    total_size: u64,
    max_segments: usize,
    min_segment_size: u64,
    _max_segment_size: u64,
) -> Vec<ByteRange> {
    if total_size == 0 {
        return vec![];
    }

    // Calculate how many segments we can actually use.
    let max_by_min = total_size
        .checked_div(min_segment_size)
        .map(|n| usize::try_from(n.max(1)).unwrap_or(usize::MAX))
        .unwrap_or(max_segments);
    let num_segments = max_segments.min(max_by_min).max(1);

    let num_segments = num_segments.min(usize::try_from(total_size).unwrap_or(usize::MAX));

    // Distribute bytes as evenly as possible.
    let base_size = total_size / num_segments as u64;
    let remainder = (total_size % num_segments as u64) as usize;

    let mut ranges = Vec::with_capacity(num_segments);
    let mut offset = 0u64;

    for i in 0..num_segments {
        let extra = if i < remainder { 1u64 } else { 0u64 };
        let segment_size = base_size + extra;
        if segment_size == 0 {
            break;
        }
        let end = offset + (segment_size - 1);
        ranges.push(ByteRange { start: offset, end });
        offset = end + 1;
    }

    // Safety check: ensure last range covers through total_size - 1.
    if let Some(last) = ranges.last_mut() {
        last.end = total_size - 1;
    }

    ranges
}

/// Validates that a set of ranges is non-overlapping and covers [0, total_size-1].
///
/// Returns `Ok(())` if valid, or `Err` with a description of the violation.
pub fn validate_ranges(ranges: &[ByteRange], total_size: u64) -> Result<(), String> {
    if total_size == 0 {
        return if ranges.is_empty() {
            Ok(())
        } else {
            Err("non-empty ranges for zero-size file".to_string())
        };
    }

    if ranges.is_empty() {
        return Err("no ranges for non-zero file".to_string());
    }

    // Check first range starts at 0.
    if ranges[0].start != 0 {
        return Err(format!(
            "first range starts at {} instead of 0",
            ranges[0].start
        ));
    }

    // Check last range ends at total_size - 1.
    let last = &ranges[ranges.len() - 1];
    if last.end != total_size - 1 {
        return Err(format!(
            "last range ends at {} instead of {}",
            last.end,
            total_size - 1
        ));
    }

    // Check ordering and non-overlap.
    for window in ranges.windows(2) {
        let a = &window[0];
        let b = &window[1];

        if a.end >= b.start {
            return Err(format!(
                "ranges overlap or out of order: [{}, {}] and [{}, {}]",
                a.start, a.end, b.start, b.end
            ));
        }

        if a.end + 1 != b.start {
            return Err(format!(
                "gap between ranges: [{}, {}] and [{}, {}]",
                a.start, a.end, b.start, b.end
            ));
        }
    }

    // Check each range is valid.
    for r in ranges {
        if r.start > r.end {
            return Err(format!("invalid range: [{}, {}]", r.start, r.end));
        }
    }

    Ok(())
}

/// Attempts to split a segment into two at its midpoint for work stealing.
///
/// Returns `None` if the remaining bytes are below `min_segment_size * 2`
/// (both halves must be at least `min_segment_size`).
///
/// # Arguments
///
/// * `current_offset` - The current download position within the segment.
/// * `segment_end` - The end of the segment (inclusive).
/// * `min_segment_size` - Minimum size for each resulting half.
pub fn split_segment(
    current_offset: u64,
    segment_end: u64,
    min_segment_size: u64,
) -> Option<(ByteRange, ByteRange)> {
    if current_offset > segment_end {
        return None;
    }

    let remaining = (segment_end - current_offset).checked_add(1)?;
    if remaining < min_segment_size.max(1).checked_mul(2)? {
        return None;
    }

    let midpoint = current_offset + remaining / 2;
    let first = ByteRange {
        start: current_offset,
        end: midpoint - 1,
    };
    let second = ByteRange {
        start: midpoint,
        end: segment_end,
    };

    Some((first, second))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    proptest! {
        #[test]
        fn allocations_cover_without_overlap(size in any::<u64>(),cap in 1usize..129,min in 1u64..1_000_000,max in 1u64..1_000_000) {
            let ranges=allocate_ranges(size,cap,min,max);
            prop_assert!(ranges.len()<=cap);
            prop_assert!(validate_ranges(&ranges,size).is_ok());
            prop_assert_eq!(ranges.iter().map(ByteRange::length).sum::<u64>(),size);
        }
        #[test]
        fn splits_conserve_ownership(start in 0u64..u64::MAX/2,len in 2u64..u64::MAX/2) {
            let end=start+len-1;
            let (a,b)=split_segment(start,end,1).unwrap();
            prop_assert!(!a.overlaps(&b));prop_assert!(a.is_adjacent_before(&b));prop_assert_eq!(a.length()+b.length(),len);
        }
    }
    #[test]
    fn limits_and_extreme_arithmetic() {
        assert_eq!(allocate_ranges(1_000_000, 2, 100, 1).len(), 2);
        assert!(validate_ranges(&allocate_ranges(u64::MAX, 8, 1, 1), u64::MAX).is_ok());
        assert!(!ByteRange {
            start: u64::MAX,
            end: u64::MAX
        }
        .is_adjacent_before(&ByteRange { start: 0, end: 0 }));
        assert!(split_segment(0, 0, 0).is_none());
        assert!(split_segment(0, 100, u64::MAX).is_none());
        assert!(ByteRange::new(0, u64::MAX).is_none());
        assert_eq!(allocate_ranges(10, 8, 8, 1).len(), 1);
    }

    #[test]
    fn test_byte_range_basic() {
        let r = ByteRange::new(0, 99).unwrap();
        assert_eq!(r.length(), 100);
        assert!(ByteRange::new(100, 50).is_none());
    }

    #[test]
    fn test_byte_range_overlap() {
        let a = ByteRange::new(0, 49).unwrap();
        let b = ByteRange::new(50, 99).unwrap();
        let c = ByteRange::new(25, 75).unwrap();

        assert!(!a.overlaps(&b));
        assert!(a.overlaps(&c));
        assert!(b.overlaps(&c));
    }

    #[test]
    fn test_allocate_single_segment() {
        let ranges = allocate_ranges(1000, 1, 100, 10000);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start, 0);
        assert_eq!(ranges[0].end, 999);
        validate_ranges(&ranges, 1000).unwrap();
    }

    #[test]
    fn test_allocate_multiple_segments() {
        let ranges = allocate_ranges(1000, 4, 100, 10000);
        assert_eq!(ranges.len(), 4);
        validate_ranges(&ranges, 1000).unwrap();

        // Each segment should be 250 bytes.
        for r in &ranges {
            assert_eq!(r.length(), 250);
        }
    }

    #[test]
    fn test_allocate_uneven_distribution() {
        let ranges = allocate_ranges(1003, 4, 100, 10000);
        validate_ranges(&ranges, 1003).unwrap();

        // 1003 / 4 = 250 remainder 3, so first 3 get 251, last gets 250.
        let total: u64 = ranges.iter().map(|r| r.length()).sum();
        assert_eq!(total, 1003);
    }

    #[test]
    fn test_allocate_respects_min_segment() {
        // 500 bytes with min 200 → at most 2 segments.
        let ranges = allocate_ranges(500, 10, 200, 10000);
        assert!(ranges.len() <= 2);
        validate_ranges(&ranges, 500).unwrap();
    }

    #[test]
    fn test_allocate_zero_size() {
        let ranges = allocate_ranges(0, 4, 100, 10000);
        assert!(ranges.is_empty());
        validate_ranges(&ranges, 0).unwrap();
    }

    #[test]
    fn test_allocate_tiny_file() {
        let ranges = allocate_ranges(1, 4, 100, 10000);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start, 0);
        assert_eq!(ranges[0].end, 0);
        validate_ranges(&ranges, 1).unwrap();
    }

    #[test]
    fn test_split_segment() {
        let result = split_segment(0, 999, 100);
        let (first, second) = result.unwrap();
        assert_eq!(first.start, 0);
        assert_eq!(first.end, 499);
        assert_eq!(second.start, 500);
        assert_eq!(second.end, 999);
    }

    #[test]
    fn test_split_segment_too_small() {
        assert!(split_segment(0, 99, 100).is_none());
    }

    #[test]
    fn test_validate_ranges_detects_overlap() {
        let ranges = vec![
            ByteRange { start: 0, end: 50 },
            ByteRange { start: 50, end: 99 },
        ];
        assert!(validate_ranges(&ranges, 100).is_err());
    }

    #[test]
    fn test_validate_ranges_detects_gap() {
        let ranges = vec![
            ByteRange { start: 0, end: 48 },
            ByteRange { start: 50, end: 99 },
        ];
        assert!(validate_ranges(&ranges, 100).is_err());
    }
}
