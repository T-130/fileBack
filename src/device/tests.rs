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

use super::*;
use tempfile::TempDir;

fn options() -> ScanOptions {
    ScanOptions {
        require_distinct_device: false,
        max_file_size: 1024 * 1024,
    }
}

fn jpeg_fixture() -> &'static [u8] {
    b"\xff\xd8\
      \xff\xc0\x00\x0b\x08\x00\x01\x00\x01\x01\x01\x11\x00\
      \xff\xda\x00\x08\x01\x01\x00\x00\x3f\x00\
      \x00\xff\xd9"
}

#[test]
fn scan_only_records_candidates_without_writing_files() {
    let temp = TempDir::new().expect("temp directory");
    let source = temp.path().join("device.img");
    let mut fixture = b"prefix".to_vec();
    fixture.extend_from_slice(jpeg_fixture());
    fixture.extend_from_slice(b"gap%PDF-1.7\nbody\n%%EOF");
    fs::write(&source, &fixture).expect("fixture");
    let file = File::open(&source).expect("source");

    let report = scan_file_ranges(
        file,
        &source,
        fixture.len() as u64,
        options(),
        ScanHooks::default(),
    )
    .expect("scan");

    assert_eq!(report.candidates.len(), 2);
    assert_eq!(fs::read_dir(temp.path()).expect("directory").count(), 1);
    assert_eq!(fs::read(&source).expect("source"), fixture);
}

#[test]
fn rejects_false_jpeg_signature_without_hiding_the_next_valid_image() {
    let temp = TempDir::new().expect("temp directory");
    let source = temp.path().join("device.img");
    let invalid = b"\xff\xd8\xff\xe0jpeg-data\xff\xd9";
    let mut fixture = b"prefix".to_vec();
    fixture.extend_from_slice(invalid);
    let valid_offset = fixture.len() as u64;
    fixture.extend_from_slice(jpeg_fixture());
    fs::write(&source, &fixture).expect("fixture");
    let file = File::open(&source).expect("source");

    let report = scan_file_ranges(
        file,
        &source,
        fixture.len() as u64,
        options(),
        ScanHooks::default(),
    )
    .expect("scan");

    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].kind, FileKind::Jpeg);
    assert_eq!(report.candidates[0].source_offset, valid_offset);
    assert_eq!(report.candidates[0].size, jpeg_fixture().len() as u64);
}

#[test]
fn cancel_returns_partial_report() {
    let temp = TempDir::new().expect("temp directory");
    let source = temp.path().join("device.img");
    fs::write(&source, b"prefix\xff\xd8\xffjpeg-data\xff\xd9").expect("fixture");
    let file = File::open(&source).expect("source");
    let hooks = ScanHooks {
        progress: None,
        control: Some(&|| ScanControl::Cancel),
        candidate: None,
    };

    let report = scan_file_ranges(file, &source, 29, options(), hooks).expect("partial report");
    assert!(report.canceled);
    assert!(report.candidates.is_empty());
}

#[test]
fn parallel_scan_reports_full_progress() {
    use std::sync::Mutex;
    let temp = TempDir::new().expect("temp directory");
    let source = temp.path().join("device.img");
    let mut fixture = b"prefix".to_vec();
    fixture.extend_from_slice(jpeg_fixture());
    fixture.extend_from_slice(b"gap%PDF-1.7\nbody\n%%EOF");
    fs::write(&source, &fixture).expect("fixture");
    let file = File::open(&source).expect("source");
    let last = Mutex::new(0_u64);
    let hooks = ScanHooks {
        progress: Some(&|done| *last.lock().expect("lock") = done),
        control: None,
        candidate: None,
    };

    let report =
        scan_file_ranges(file, &source, fixture.len() as u64, options(), hooks).expect("scan");

    assert_eq!(report.candidates.len(), 2);
    assert_eq!(*last.lock().expect("lock"), fixture.len() as u64);
}

#[test]
fn source_analysis_failure_does_not_block_selected_recovery() {
    let temp = TempDir::new().expect("temp directory");
    let source = temp.path().join("device.img");
    let output = temp.path().join("recovered");
    let mut fixture = b"prefix".to_vec();
    fixture.extend_from_slice(jpeg_fixture());
    fixture.extend_from_slice(b"gap%PDF-1.7\nbody\n%%EOF");
    fs::write(&source, &fixture).expect("fixture");
    let scan_file = File::open(&source).expect("source");
    let mut report = scan_file_ranges(
        scan_file,
        &source,
        fixture.len() as u64,
        options(),
        ScanHooks::default(),
    )
    .expect("scan");
    report.source_analysis.detail = Some("synthetic analysis failure".into());
    let selected = vec![report.candidates[1].clone()];
    let recovery_file = File::open(&source).expect("source");

    let recovered = recover_candidates_for_test(
        recovery_file,
        &source,
        fixture.len() as u64,
        &selected,
        &output,
    )
    .expect("recover");

    assert_eq!(recovered.files.len(), 1);
    assert_eq!(recovered.files[0].kind, FileKind::Pdf);
    assert_eq!(
        recovered.files[0].path,
        output.join("documents/fileback_000001.pdf")
    );
    assert_eq!(fs::read_dir(&output).expect("output").count(), 1);
}

#[test]
fn detects_header_crossing_scan_block_boundary() {
    let temp = TempDir::new().expect("temp directory");
    let source = temp.path().join("device.img");
    let start = SCAN_BLOCK_SIZE - 3;
    let mut fixture = vec![0_u8; SCAN_BLOCK_SIZE + 64];
    let png = b"\x89PNG\r\n\x1a\npng-dataIEND\xaeB`\x82";
    fixture[start..start + png.len()].copy_from_slice(png);
    fs::write(&source, fixture).expect("fixture");
    let file = File::open(&source).expect("source");

    let report = scan_file_ranges(
        file,
        &source,
        (SCAN_BLOCK_SIZE + 64) as u64,
        options(),
        ScanHooks::default(),
    )
    .expect("scan");

    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].source_offset, start as u64);
    assert_eq!(report.candidates[0].kind, FileKind::Png);
}

#[test]
fn detects_header_crossing_chunk_boundary() {
    use std::os::unix::fs::FileExt;

    let temp = TempDir::new().expect("temp directory");
    let source = temp.path().join("device.img");
    let start = CHUNK_SIZE - 3;
    let fixture = File::create(&source).expect("fixture");
    fixture.set_len(CHUNK_SIZE + 64).expect("size");
    fixture
        .write_all_at(b"\x89PNG\r\n\x1a\npng-dataIEND\xaeB`\x82", start)
        .expect("png");
    drop(fixture);
    let file = File::open(&source).expect("source");

    let report = scan_file_ranges(
        file,
        &source,
        CHUNK_SIZE + 64,
        options(),
        ScanHooks::default(),
    )
    .expect("scan");

    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].source_offset, start);
    assert_eq!(report.candidates[0].kind, FileKind::Png);
}

#[test]
fn truncated_zip_does_not_abort_scan() {
    let temp = TempDir::new().expect("temp directory");
    let source = temp.path().join("device.img");
    let fixture = b"prefixPK\x03\x04zip-dataPK\x05\x06";
    fs::write(&source, fixture).expect("fixture");
    let file = File::open(&source).expect("source");

    let report = scan_file_ranges(
        file,
        &source,
        fixture.len() as u64,
        options(),
        ScanHooks::default(),
    )
    .expect("truncated candidate should not abort scan");

    assert!(report.candidates.is_empty());
    assert_eq!(report.incomplete_candidates, 1);
}

#[test]
fn cancel_interrupts_carving_between_blocks() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let temp = TempDir::new().expect("temp directory");
    let source = temp.path().join("device.img");
    let mut fixture = vec![0_u8; CARVE_BLOCK_SIZE + 64];
    fixture[..3].copy_from_slice(FileKind::Jpeg.header());
    fs::write(&source, fixture).expect("fixture");
    let file = File::open(&source).expect("source");
    let checks = AtomicUsize::new(0);
    let control = || {
        if checks.fetch_add(1, Ordering::Relaxed) == 0 {
            ScanControl::Continue
        } else {
            ScanControl::Cancel
        }
    };
    let hooks = ScanHooks {
        progress: None,
        control: Some(&control),
        candidate: None,
    };
    let mut options = options();
    options.max_file_size = 2 * CARVE_BLOCK_SIZE;

    let report = scan_file_ranges(
        file,
        &source,
        (CARVE_BLOCK_SIZE + 64) as u64,
        options,
        hooks,
    )
    .expect("partial report");

    assert!(report.canceled);
    assert!(report.candidates.is_empty());
    assert_eq!(report.incomplete_candidates, 0);
}

#[test]
fn rejects_source_size_beyond_actual_data() {
    let temp = TempDir::new().expect("temp directory");
    let source = temp.path().join("device.img");
    fs::write(&source, b"short image").expect("fixture");
    let file = File::open(&source).expect("source");

    let error = scan_file_ranges(file, &source, 4096, options(), ScanHooks::default())
        .expect_err("short source must fail");

    assert!(error.to_string().contains("提前到达末尾"));
}

#[test]
fn failed_range_read_does_not_publish_partial_file() {
    let temp = TempDir::new().expect("temp directory");
    let source = temp.path().join("device.img");
    fs::write(&source, b"short").expect("fixture");
    let file = File::open(&source).expect("source");
    let output = temp.path().join("recovered.jpg");

    write_range(&file, 0, 100, &output).expect_err("short source must fail");

    assert!(!output.exists());
    assert_eq!(fs::read_dir(temp.path()).expect("directory").count(), 1);
}

#[test]
fn range_recovery_does_not_replace_existing_file() {
    let temp = TempDir::new().expect("temp directory");
    let source = temp.path().join("device.img");
    fs::write(&source, b"complete source data").expect("fixture");
    let file = File::open(&source).expect("source");
    let output = temp.path().join("recovered.jpg");
    fs::write(&output, b"keep me").expect("existing output");

    write_range(&file, 0, 20, &output).expect_err("existing output must fail");

    assert_eq!(fs::read(&output).expect("existing output"), b"keep me");
    assert_eq!(fs::read_dir(temp.path()).expect("directory").count(), 2);
}

fn recover_candidates_for_test(
    file: File,
    source: &Path,
    source_size: u64,
    candidates: &[DeviceCandidate],
    output: &Path,
) -> Result<ScanReport> {
    prepare_output_directory(output)?;
    let mut files = Vec::new();
    for candidate in candidates {
        let output_path =
            crate::recovery::output_file_path(output, candidate.kind, files.len() + 1)?;
        write_range(
            &file,
            candidate.source_offset,
            candidate.source_offset + candidate.size,
            &output_path,
        )?;
        files.push(RecoveredFile {
            path: output_path,
            kind: candidate.kind,
            size: candidate.size,
            source_offset: candidate.source_offset,
        });
    }
    Ok(ScanReport {
        source: source.to_path_buf(),
        output_dir: output.to_path_buf(),
        scanned_bytes: source_size,
        files,
        incomplete_candidates: 0,
        oversized_candidates: 0,
    })
}

#[test]
fn detects_riff_header_crossing_scan_block_boundary() {
    let temp = TempDir::new().expect("temp directory");
    let source = temp.path().join("device.img");
    let start = SCAN_BLOCK_SIZE - 5;
    let payload = b"synthetic-wave";
    let mut wave = b"RIFF".to_vec();
    wave.extend_from_slice(&((payload.len() + 4) as u32).to_le_bytes());
    wave.extend_from_slice(b"WAVE");
    wave.extend_from_slice(payload);
    let mut fixture = vec![0_u8; SCAN_BLOCK_SIZE + 64];
    fixture[start..start + wave.len()].copy_from_slice(&wave);
    fs::write(&source, fixture).expect("fixture");
    let file = File::open(&source).expect("source");

    let report = scan_file_ranges(
        file,
        &source,
        (SCAN_BLOCK_SIZE + 64) as u64,
        options(),
        ScanHooks::default(),
    )
    .expect("scan");

    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].source_offset, start as u64);
    assert_eq!(report.candidates[0].kind, FileKind::Wav);
    assert_eq!(report.candidates[0].size, wave.len() as u64);
}

#[test]
fn physical_scan_classifies_docx_from_zip_directory() {
    let temp = TempDir::new().expect("temp directory");
    let source = temp.path().join("device.img");
    let names = [b"[Content_Types].xml".as_slice(), b"word/document.xml"];
    let mut fixture = Vec::new();
    for name in names {
        let mut header = vec![0_u8; 30];
        header[..4].copy_from_slice(b"PK\x03\x04");
        header[26..28].copy_from_slice(&(name.len() as u16).to_le_bytes());
        fixture.extend_from_slice(&header);
        fixture.extend_from_slice(name);
    }
    let directory_offset = fixture.len();
    for name in names {
        let mut header = vec![0_u8; 46];
        header[..4].copy_from_slice(b"PK\x01\x02");
        header[28..30].copy_from_slice(&(name.len() as u16).to_le_bytes());
        fixture.extend_from_slice(&header);
        fixture.extend_from_slice(name);
    }
    let directory_size = fixture.len() - directory_offset;
    let mut end = vec![0_u8; 22];
    end[..4].copy_from_slice(b"PK\x05\x06");
    end[8..10].copy_from_slice(&2_u16.to_le_bytes());
    end[10..12].copy_from_slice(&2_u16.to_le_bytes());
    end[12..16].copy_from_slice(&(directory_size as u32).to_le_bytes());
    end[16..20].copy_from_slice(&(directory_offset as u32).to_le_bytes());
    fixture.extend_from_slice(&end);
    fs::write(&source, &fixture).expect("fixture");
    let file = File::open(&source).expect("source");

    let report = scan_file_ranges(
        file,
        &source,
        fixture.len() as u64,
        options(),
        ScanHooks::default(),
    )
    .expect("scan");

    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].kind, FileKind::Docx);
    assert_eq!(report.candidates[0].size, fixture.len() as u64);
}

#[test]
fn preview_reads_only_requested_candidate_range_without_creating_outputs() {
    let temp = TempDir::new().expect("temp directory");
    let source = temp.path().join("device.img");
    let image = image::DynamicImage::new_rgba8(320, 180);
    let mut png = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut png, image::ImageFormat::Png)
        .expect("PNG fixture");
    let mut fixture = b"prefix".to_vec();
    fixture.extend_from_slice(png.get_ref());
    fixture.extend_from_slice(b"gap%PDF-1.7\nbody\n%%EOF");
    fs::write(&source, &fixture).expect("fixture");
    let file = File::open(&source).expect("source");
    let report = scan_file_ranges(
        file,
        &source,
        fixture.len() as u64,
        options(),
        ScanHooks::default(),
    )
    .expect("scan");
    let selected = vec![report.candidates[0].clone()];
    let preview_file = File::open(&source).expect("source");

    let previews =
        preview_candidate_ranges(&preview_file, fixture.len() as u64, &selected).expect("preview");

    assert_eq!(previews.len(), 1);
    assert_eq!(previews[0].id, selected[0].id);
    assert_eq!(previews[0].details, ["图片尺寸 320 × 180"]);
    assert!(previews[0].thumbnail_png.is_some());
    assert_eq!(fs::read_dir(temp.path()).expect("directory").count(), 1);
}
