// Copyright 2026 zyy, zjon234@gmail.com
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use super::{
    ContainerEvidence, Location, RangeSet, ViewEvidence, analyze_disk,
    apfs::{ExactStream, physical_extent_range},
    classify_candidate,
    gpt::{APFS_PARTITION_TYPE, Partition, PartitionView, apfs_partitions},
};
use crate::recovery::{CandidateSourceState, DeviceCandidate, DeviceScanReport, FileKind};
use std::{
    collections::HashMap,
    io::{Cursor, Read, Seek, SeekFrom},
};

const SECTOR: usize = 512;
const DISK_SECTORS: usize = 2048;

fn candidate(id: &str, source_offset: u64, size: u64) -> DeviceCandidate {
    DeviceCandidate {
        id: id.into(),
        kind: FileKind::Png,
        size,
        source_offset,
    }
}

fn gpt_image(partitions: &[(u64, u64)]) -> Vec<u8> {
    let mut image = vec![0_u8; DISK_SECTORS * SECTOR];
    let mut entries = vec![0_u8; 128 * partitions.len()];
    for (index, &(first, last)) in partitions.iter().enumerate() {
        let entry = &mut entries[index * 128..(index + 1) * 128];
        entry[..16].copy_from_slice(&APFS_PARTITION_TYPE);
        entry[32..40].copy_from_slice(&first.to_le_bytes());
        entry[40..48].copy_from_slice(&last.to_le_bytes());
    }
    image[2 * SECTOR..2 * SECTOR + entries.len()].copy_from_slice(&entries);

    let header = &mut image[SECTOR..2 * SECTOR];
    header[..8].copy_from_slice(b"EFI PART");
    header[8..12].copy_from_slice(&0x0001_0000_u32.to_le_bytes());
    header[12..16].copy_from_slice(&92_u32.to_le_bytes());
    header[24..32].copy_from_slice(&1_u64.to_le_bytes());
    header[32..40].copy_from_slice(&((DISK_SECTORS - 1) as u64).to_le_bytes());
    header[40..48].copy_from_slice(&34_u64.to_le_bytes());
    header[48..56].copy_from_slice(&((DISK_SECTORS - 34) as u64).to_le_bytes());
    header[72..80].copy_from_slice(&2_u64.to_le_bytes());
    header[80..84].copy_from_slice(&(partitions.len() as u32).to_le_bytes());
    header[84..88].copy_from_slice(&128_u32.to_le_bytes());
    header[88..92].copy_from_slice(&crc32fast::hash(&entries).to_le_bytes());
    reseal_gpt(&mut image);
    image
}

fn reseal_gpt(image: &mut [u8]) {
    image[SECTOR + 16..SECTOR + 20].fill(0);
    let crc = crc32fast::hash(&image[SECTOR..SECTOR + 92]);
    image[SECTOR + 16..SECTOR + 20].copy_from_slice(&crc.to_le_bytes());
}

#[test]
fn finds_apfs_partition_with_exact_disk_offsets() {
    let image = gpt_image(&[(40, 199)]);
    let mut reader = Cursor::new(image);

    let partitions =
        apfs_partitions(&mut reader, (DISK_SECTORS * SECTOR) as u64).expect("valid synthetic GPT");

    assert_eq!(partitions.len(), 1);
    assert_eq!(partitions[0].start, 40 * 512);
    assert_eq!(partitions[0].end, 200 * 512);
    assert_eq!(partitions[0].len, 160 * 512);
}

#[test]
fn gpt_rejects_header_and_entry_crc_damage() {
    let mut bad_header = gpt_image(&[(40, 199)]);
    bad_header[SECTOR + 16] ^= 1;
    assert!(apfs_partitions(&mut Cursor::new(bad_header), (DISK_SECTORS * SECTOR) as u64).is_err());

    let mut bad_entries = gpt_image(&[(40, 199)]);
    bad_entries[2 * SECTOR + 32] ^= 1;
    assert!(
        apfs_partitions(
            &mut Cursor::new(bad_entries),
            (DISK_SECTORS * SECTOR) as u64
        )
        .is_err()
    );
}

#[test]
fn gpt_rejects_out_of_bounds_and_overflowing_partitions() {
    let outside = gpt_image(&[(40, 5000)]);
    assert!(apfs_partitions(&mut Cursor::new(outside), (DISK_SECTORS * SECTOR) as u64).is_err());

    let mut overflow = gpt_image(&[(u64::MAX, u64::MAX)]);
    overflow[SECTOR + 40..SECTOR + 48].copy_from_slice(&0_u64.to_le_bytes());
    overflow[SECTOR + 48..SECTOR + 56].copy_from_slice(&u64::MAX.to_le_bytes());
    reseal_gpt(&mut overflow);
    assert!(apfs_partitions(&mut Cursor::new(overflow), (DISK_SECTORS * SECTOR) as u64).is_err());
}

#[test]
fn gpt_rejects_invalid_entry_geometry_and_overlap() {
    let mut invalid = gpt_image(&[(40, 199)]);
    invalid[SECTOR + 84..SECTOR + 88].copy_from_slice(&64_u32.to_le_bytes());
    reseal_gpt(&mut invalid);
    assert!(apfs_partitions(&mut Cursor::new(invalid), (DISK_SECTORS * SECTOR) as u64).is_err());

    let overlap = gpt_image(&[(40, 199), (100, 299)]);
    assert!(apfs_partitions(&mut Cursor::new(overlap), (DISK_SECTORS * SECTOR) as u64).is_err());
}

#[test]
fn partition_view_enforces_bounds_for_reads_and_seeks() {
    let bytes: Vec<u8> = (0..30).collect();
    let partition = Partition {
        start: 10,
        end: 20,
        len: 10,
    };
    let mut cursor = Cursor::new(bytes);
    let mut view = PartitionView::new(&mut cursor, &partition);
    view.seek(SeekFrom::Start(2))
        .expect("seek within partition");
    let mut data = [0_u8; 4];
    view.read_exact(&mut data).expect("bounded read");
    assert_eq!(data, [12, 13, 14, 15]);

    view.seek(SeekFrom::Start(8)).expect("seek near end");
    let mut tail = [0_u8; 4];
    assert_eq!(view.read(&mut tail).expect("read tail"), 2);
    assert_eq!(&tail[..2], &[18, 19]);
    assert!(view.seek(SeekFrom::Start(11)).is_err());
    view.seek(SeekFrom::End(-1)).expect("end-relative seek");
    assert_eq!(view.stream_position().expect("position"), 9);
}

#[test]
fn ranges_cover_only_complete_candidate_spans() {
    let mut ranges = RangeSet::default();
    ranges.add(2000, 3000);
    ranges.add(1000, 2000);
    ranges.add(3000, 4000);
    ranges.add(4000, 5000);
    ranges.normalize();

    assert!(ranges.covers(1000, 5000));
    assert!(ranges.covers(1500, 1600));
    assert!(!ranges.covers(999, 1001));
    assert!(ranges.overlaps(4500, 6000));
    assert!(!ranges.overlaps(5000, 6000));
    assert!(!ranges.covers(1000, 1000));
}

fn container_evidence() -> ContainerEvidence {
    ContainerEvidence {
        current: ViewEvidence::default(),
        snapshots: ViewEvidence::default(),
        free: None,
        container_size: 100_000,
        warnings: Vec::new(),
    }
}

#[test]
fn classifies_current_snapshot_deleted_partial_and_unknown() {
    let candidate = candidate("candidate", 1200, 100);
    let mut evidence = container_evidence();
    evidence.current.ranges.add(1000, 2000);
    evidence.current.ranges.normalize();
    evidence.current.locations.insert(
        candidate.id.clone(),
        Some(Location {
            volume: "Data".into(),
            path: "/Photos/photo.png".into(),
            snapshot: None,
        }),
    );
    let current = classify_candidate(&candidate, &evidence);
    assert_eq!(current.state, CandidateSourceState::Present);
    assert_eq!(current.path.as_deref(), Some("/Photos/photo.png"));

    evidence.current = ViewEvidence::default();
    evidence.snapshots.ranges.add(1000, 2000);
    evidence.snapshots.ranges.normalize();
    evidence.free = Some(RangeSet::default());
    evidence.free.as_mut().expect("free ranges").add(1000, 2000);
    let snapshot = classify_candidate(&candidate, &evidence);
    assert_eq!(snapshot.state, CandidateSourceState::Snapshot);

    evidence.snapshots = ViewEvidence::default();
    let deleted = classify_candidate(&candidate, &evidence);
    assert_eq!(deleted.state, CandidateSourceState::ProbablyDeleted);
    assert!(deleted.evidence.contains("不能排除"));

    evidence.free = Some(RangeSet::default());
    evidence.current.ranges.add(1000, 1250);
    evidence.current.ranges.normalize();
    let partial = classify_candidate(&candidate, &evidence);
    assert_eq!(partial.state, CandidateSourceState::Unknown);
    assert!(partial.evidence.contains("部分"));

    evidence.current = ViewEvidence::default();
    evidence.free = None;
    assert_eq!(
        classify_candidate(&candidate, &evidence).state,
        CandidateSourceState::Unknown
    );
}

#[test]
fn ambiguous_exact_paths_are_not_presented_as_original_name() {
    let id = "candidate".to_string();
    let first = ViewEvidence {
        ranges: RangeSet::default(),
        locations: HashMap::from([(
            id.clone(),
            Some(Location {
                volume: "Data".into(),
                path: "/one.png".into(),
                snapshot: None,
            }),
        )]),
    };
    let second = ViewEvidence {
        ranges: RangeSet::default(),
        locations: HashMap::from([(
            id.clone(),
            Some(Location {
                volume: "Data".into(),
                path: "/two.png".into(),
                snapshot: None,
            }),
        )]),
    };
    let mut merged = ViewEvidence::default();
    merged.merge(first);
    merged.merge(second);

    assert_eq!(merged.locations.get(&id), Some(&None));
}

#[test]
fn sparse_fragmented_and_out_of_bounds_extents_are_conservative() {
    assert!(physical_extent_range(0, 4096, 10_000, 4096, 100_000).is_err());
    assert!(physical_extent_range(3, 4096, 10_000, 4096, 8192).is_err());
    assert_eq!(
        physical_extent_range(2, 4096, 10_000, 4096, 100_000).expect("valid extent"),
        (18_192, 22_288)
    );

    let mut contiguous = ExactStream::new(6000);
    contiguous.push_raw(0, 4096, (10_000, 14_096));
    contiguous.push_raw(4096, 4096, (14_096, 18_192));
    assert_eq!(contiguous.start(), Some(10_000));

    let mut fragmented = ExactStream::new(6000);
    fragmented.push_raw(0, 4096, (10_000, 14_096));
    fragmented.push_raw(4096, 4096, (20_000, 24_096));
    assert_eq!(fragmented.start(), None);

    let mut logical_gap = ExactStream::new(6000);
    logical_gap.push_raw(0, 4096, (10_000, 14_096));
    logical_gap.push_raw(8192, 4096, (14_096, 18_192));
    assert_eq!(logical_gap.start(), None);
}

#[test]
fn unsupported_or_damaged_disk_keeps_every_candidate_unknown() {
    let candidates = vec![candidate("one", 100, 20), candidate("two", 200, 30)];
    let analysis = analyze_disk(
        Cursor::new(vec![0_u8; DISK_SECTORS * SECTOR]),
        (DISK_SECTORS * SECTOR) as u64,
        &candidates,
    );

    assert!(!analysis.complete);
    assert!(analysis.detail.is_some());
    assert_eq!(analysis.candidates.len(), candidates.len());
    assert!(
        analysis
            .candidates
            .values()
            .all(|source| source.state == CandidateSourceState::Unknown)
    );
}

#[test]
fn legacy_report_deserializes_with_unknown_source_analysis() {
    let report: DeviceScanReport = serde_json::from_value(serde_json::json!({
        "source": "/dev/rdisk2",
        "source_size": 4096,
        "candidates": [],
        "incomplete_candidates": 0,
        "oversized_candidates": 0
    }))
    .expect("legacy report");

    assert!(report.source_analysis.candidates.is_empty());
    assert!(!report.source_analysis.complete);
    assert!(report.source_analysis.detail.is_none());
}
