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

fn fixture() -> Vec<u8> {
    let mut image = b"unallocated-space".to_vec();
    image.extend_from_slice(jpeg_fixture());
    image.extend_from_slice(b"gap");
    image.extend_from_slice(b"\x89PNG\r\n\x1a\npng-dataIEND\xaeB`\x82");
    image.extend_from_slice(b"%PDF-1.7\nbody\n%%EOF");
    image.extend_from_slice(b"PK\x03\x04zip-dataPK\x05\x06");
    image.extend_from_slice(&[0; 18]);
    image
}

#[test]
fn recovers_complete_files_without_modifying_source() {
    let temp = TempDir::new().expect("temp directory");
    let source = temp.path().join("image.raw");
    let output = temp.path().join("recovered");
    let original = fixture();
    fs::write(&source, &original).expect("fixture");

    let report = scan_image(&source, &output, options()).expect("scan");

    assert_eq!(report.files.len(), 4);
    assert_eq!(report.scanned_bytes, original.len() as u64);
    assert_eq!(report.incomplete_candidates, 0);
    assert_eq!(fs::read(&source).expect("source"), original);

    let mut extensions: Vec<_> = report
        .files
        .iter()
        .map(|file| file.kind.extension())
        .collect();
    extensions.sort_unstable();
    assert_eq!(extensions, ["jpg", "pdf", "png", "zip"]);
    for file in report.files {
        assert_eq!(fs::metadata(file.path).expect("output").len(), file.size);
    }
}

#[test]
fn rejects_false_jpeg_signature_and_keeps_standard_encoded_jpeg() {
    let temp = TempDir::new().expect("temp directory");
    let source = temp.path().join("image.raw");
    let output = temp.path().join("recovered");
    let invalid = b"\xff\xd8\xff\xe0jpeg-data\xff\xd9";
    let mut jpeg = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 2)
        .write_to(&mut jpeg, image::ImageFormat::Jpeg)
        .expect("JPEG fixture");
    let jpeg = jpeg.into_inner();
    let mut data = invalid.to_vec();
    data.extend_from_slice(&jpeg);
    fs::write(&source, data).expect("fixture");

    let report = scan_image(&source, &output, options()).expect("scan");

    assert_eq!(report.files.len(), 1);
    assert_eq!(report.files[0].kind, FileKind::Jpeg);
    assert_eq!(report.files[0].source_offset, invalid.len() as u64);
    assert_eq!(fs::read(&report.files[0].path).expect("JPEG"), jpeg);
}

#[test]
fn rejects_short_or_incomplete_jpeg_segments_without_panicking() {
    assert_eq!(
        format::jpeg_header_is_valid(b"\xff\xd8\xff\xda\x00\x02"),
        Some(false)
    );
    assert_eq!(format::jpeg_header_is_valid(&jpeg_fixture()[..5]), None);
}

#[test]
fn rejects_non_empty_output_directory() {
    let temp = TempDir::new().expect("temp directory");
    let source = temp.path().join("image.raw");
    fs::write(&source, fixture()).expect("fixture");
    fs::write(temp.path().join("existing"), b"keep me").expect("existing file");

    let error = scan_image(&source, temp.path(), options()).expect_err("must fail");
    assert!(error.to_string().contains("必须为空"));
}

#[test]
fn rejects_same_device_when_required() {
    let temp = TempDir::new().expect("temp directory");
    let source = temp.path().join("image.raw");
    let output = temp.path().join("recovered");
    fs::write(&source, fixture()).expect("fixture");

    let error = scan_image(&source, &output, ScanOptions::default()).expect_err("must fail");
    assert!(error.to_string().contains("同一设备"));
    assert!(!output.exists(), "拒绝扫描前不应创建恢复目录");
}

#[test]
fn continues_after_incomplete_candidate() {
    let temp = TempDir::new().expect("temp directory");
    let source = temp.path().join("image.raw");
    let output = temp.path().join("recovered");
    fs::write(
        &source,
        b"broken\xff\xd8\xffmissing-end%PDF-1.7\nvalid\n%%EOF",
    )
    .expect("fixture");

    let report = scan_image(&source, &output, options()).expect("scan");

    assert_eq!(report.incomplete_candidates, 1);
    assert_eq!(report.files.len(), 1);
    assert_eq!(report.files[0].kind, FileKind::Pdf);
}

#[test]
fn reports_incomplete_candidate() {
    let temp = TempDir::new().expect("temp directory");
    let source = temp.path().join("image.raw");
    let output = temp.path().join("recovered");
    fs::write(&source, b"prefix\xff\xd8\xffmissing-end").expect("fixture");

    let report = scan_image(&source, &output, options()).expect("scan");
    assert!(report.files.is_empty());
    assert_eq!(report.incomplete_candidates, 1);
}

#[test]
fn compatible_publication_preserves_existing_output() {
    let temp = TempDir::new().expect("temp directory");
    let output_path = temp.path().join("recovered.jpg");
    fs::write(&output_path, b"keep me").expect("existing output");
    let mut temporary = tempfile::NamedTempFile::new_in(temp.path()).expect("temporary output");
    temporary
        .write_all(b"new data")
        .expect("temporary contents");

    let error = persist_with_exclusive_placeholder(temporary, &output_path)
        .expect_err("existing output must be rejected");

    assert_eq!(
        error
            .root_cause()
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind),
        Some(std::io::ErrorKind::AlreadyExists)
    );
    assert_eq!(fs::read(output_path).expect("existing output"), b"keep me");
    assert_eq!(
        fs::read_dir(temp.path()).expect("directory").count(),
        1,
        "失败后应自动清理临时文件"
    );
}

#[test]
fn compatible_publication_publishes_complete_file() {
    let temp = TempDir::new().expect("temp directory");
    let output_path = temp.path().join("recovered.jpg");
    let mut temporary = tempfile::NamedTempFile::new_in(temp.path()).expect("temporary output");
    temporary
        .write_all(b"complete data")
        .expect("temporary contents");

    persist_with_exclusive_placeholder(temporary, &output_path).expect("publish output");

    assert_eq!(
        fs::read(&output_path).expect("published output"),
        b"complete data"
    );
    assert_eq!(fs::read_dir(temp.path()).expect("directory").count(), 1);
}

fn synthetic_zip(main_document: Option<&[u8]>) -> Vec<u8> {
    let names: Vec<&[u8]> = match main_document {
        Some(document) => vec![b"[Content_Types].xml", document],
        None => vec![b"readme.txt"],
    };
    let mut zip = Vec::new();
    for name in &names {
        let mut header = vec![0_u8; 30];
        header[..4].copy_from_slice(b"PK\x03\x04");
        header[26..28].copy_from_slice(&(name.len() as u16).to_le_bytes());
        zip.extend_from_slice(&header);
        zip.extend_from_slice(name);
    }
    let directory_offset = zip.len();
    for name in &names {
        let mut header = vec![0_u8; 46];
        header[..4].copy_from_slice(b"PK\x01\x02");
        header[28..30].copy_from_slice(&(name.len() as u16).to_le_bytes());
        zip.extend_from_slice(&header);
        zip.extend_from_slice(name);
    }
    let directory_size = zip.len() - directory_offset;
    let mut end = vec![0_u8; 22];
    end[..4].copy_from_slice(b"PK\x05\x06");
    end[8..10].copy_from_slice(&(names.len() as u16).to_le_bytes());
    end[10..12].copy_from_slice(&(names.len() as u16).to_le_bytes());
    end[12..16].copy_from_slice(&(directory_size as u32).to_le_bytes());
    end[16..20].copy_from_slice(&(directory_offset as u32).to_le_bytes());
    zip.extend_from_slice(&end);
    zip
}

fn iso_atom(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut atom = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    atom.extend_from_slice(kind);
    atom.extend_from_slice(payload);
    atom
}

fn iso_media(brand: &[u8; 4], metadata_first: bool) -> Vec<u8> {
    let mut ftyp_payload = brand.to_vec();
    ftyp_payload.extend_from_slice(&0_u32.to_be_bytes());
    let mut media = iso_atom(b"ftyp", &ftyp_payload);
    let metadata = iso_atom(b"moov", b"metadata");
    let contents = iso_atom(b"mdat", b"video-data");
    if metadata_first {
        media.extend_from_slice(&metadata);
        media.extend_from_slice(&contents);
    } else {
        media.extend_from_slice(&contents);
        media.extend_from_slice(&metadata);
    }
    media
}

fn riff(kind: &[u8; 4]) -> Vec<u8> {
    let payload = b"synthetic-media";
    let mut data = b"RIFF".to_vec();
    data.extend_from_slice(&((payload.len() + 4) as u32).to_le_bytes());
    data.extend_from_slice(kind);
    data.extend_from_slice(payload);
    data
}

#[test]
fn identifies_office_documents_from_zip_central_directory() {
    for (document, expected) in [
        (b"word/document.xml".as_slice(), FileKind::Docx),
        (b"xl/workbook.xml".as_slice(), FileKind::Xlsx),
        (b"ppt/presentation.xml".as_slice(), FileKind::Pptx),
    ] {
        let zip = synthetic_zip(Some(document));
        assert_eq!(format::classify_zip(&zip), expected);
    }
    assert_eq!(format::classify_zip(&synthetic_zip(None)), FileKind::Zip);
    assert_eq!(
        format::classify_zip(b"PK\x03\x04word/document.xml"),
        FileKind::Zip
    );
}

#[test]
fn locates_media_container_boundaries() {
    for metadata_first in [false, true] {
        let media = iso_media(b"isom", metadata_first);
        assert_eq!(format::iso_media_end(&media), Some(media.len()));
    }
    let mut media = iso_media(b"isom", false);
    let expected_end = media.len();
    media.extend_from_slice(b"trailing-data");
    assert_eq!(format::iso_media_end(&media), Some(expected_end));
    assert_eq!(format::iso_media_end(&media[..12]), None);

    for (brand, expected) in [
        (b"qt  ", FileKind::Mov),
        (b"M4A ", FileKind::M4a),
        (b"isom", FileKind::Mp4),
    ] {
        let media = iso_media(brand, false);
        let (_, _, kind) = format::find_next_header(&media, 0).expect("ISO header");
        assert_eq!(kind, expected);
    }
}

#[test]
fn invalid_media_signature_does_not_hide_later_valid_header() {
    let mut image = b"ftyp-invalid-WAVE".to_vec();
    let mp4_start = image.len();
    image.extend_from_slice(&iso_media(b"isom", false));
    let wav_start = image.len();
    image.extend_from_slice(&riff(b"WAVE"));

    let (start, next, kind) = format::find_next_header(&image, 0).expect("valid MP4 header");
    assert_eq!((start, kind), (mp4_start, FileKind::Mp4));
    let (start, _, kind) = format::find_next_header(&image, next).expect("valid WAV header");
    assert_eq!((start, kind), (wav_start, FileKind::Wav));
}

#[test]
fn validates_riff_lengths_and_extensions() {
    for (name, expected, extension) in [
        (b"AVI ", FileKind::Avi, "avi"),
        (b"WAVE", FileKind::Wav, "wav"),
    ] {
        let media = riff(name);
        assert_eq!(
            format::riff_file_len(&media[..12], expected),
            Some(media.len() as u64)
        );
        assert_eq!(expected.extension(), extension);
    }
    assert_eq!(
        format::riff_file_len(&riff(b"AVI ")[..12], FileKind::Wav),
        None
    );
}

#[test]
fn recovers_supported_formats_into_category_directories() {
    let temp = TempDir::new().expect("temp directory");
    let source = temp.path().join("image.raw");
    let output = temp.path().join("recovered");
    let mut image = Vec::new();
    for data in [
        synthetic_zip(Some(b"word/document.xml")),
        synthetic_zip(Some(b"xl/workbook.xml")),
        synthetic_zip(Some(b"ppt/presentation.xml")),
        iso_media(b"isom", false),
        iso_media(b"qt  ", true),
        iso_media(b"M4A ", false),
        riff(b"AVI "),
        riff(b"WAVE"),
    ] {
        image.extend_from_slice(&data);
        image.extend_from_slice(b"gap");
    }
    fs::write(&source, image).expect("fixture");

    let report = scan_image(&source, &output, options()).expect("scan");
    let expected = [
        (FileKind::Docx, "documents/fileback_000001.docx"),
        (FileKind::Xlsx, "spreadsheets/fileback_000002.xlsx"),
        (FileKind::Pptx, "presentations/fileback_000003.pptx"),
        (FileKind::Mp4, "videos/fileback_000004.mp4"),
        (FileKind::Mov, "videos/fileback_000005.mov"),
        (FileKind::M4a, "audio/fileback_000006.m4a"),
        (FileKind::Avi, "videos/fileback_000007.avi"),
        (FileKind::Wav, "audio/fileback_000008.wav"),
    ];
    assert_eq!(report.files.len(), expected.len());
    for (file, (kind, path)) in report.files.iter().zip(expected) {
        assert_eq!(file.kind, kind);
        assert_eq!(file.path, output.join(path));
        assert_eq!(fs::metadata(&file.path).expect("output").len(), file.size);
    }
}
