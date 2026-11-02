//! 当前页候选的受限预览：图片缩略图与 Office 核心元数据。

use super::{CandidatePreview, DeviceCandidate, FileKind};
use anyhow::{Context, Result, bail};
use memchr::memmem;
use std::io::{Cursor, Read};

pub(crate) const MAX_PREVIEW_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_METADATA_BYTES: u64 = 1024 * 1024;
const THUMBNAIL_WIDTH: u32 = 320;
const THUMBNAIL_HEIGHT: u32 = 180;

pub(crate) fn unavailable_preview(candidate: &DeviceCandidate, message: &str) -> CandidatePreview {
    CandidatePreview {
        id: candidate.id.clone(),
        title: None,
        details: Vec::new(),
        thumbnail_png: None,
        error: Some(message.to_string()),
    }
}

/// 预览失败不会阻止候选恢复；错误仅用于说明为什么无法辅助辨认。
pub fn inspect_candidate(candidate: &DeviceCandidate, data: &[u8]) -> CandidatePreview {
    let mut preview = unavailable_preview(candidate, "");
    preview.error = None;
    match candidate.kind {
        FileKind::Jpeg | FileKind::Png => match image_preview(data) {
            Ok((thumbnail, width, height)) => {
                preview.thumbnail_png = Some(thumbnail);
                preview.details.push(format!("图片尺寸 {width} × {height}"));
            }
            Err(_) => preview.error = Some("图片不完整或编码不受支持，无法生成缩略图".into()),
        },
        FileKind::Docx | FileKind::Xlsx | FileKind::Pptx => match office_metadata(data) {
            Ok((title, details)) => {
                preview.title = title;
                preview.details = details;
            }
            Err(_) => preview.error = Some("无法读取文档标题等元数据".into()),
        },
        _ => {}
    }
    preview
}

fn image_preview(data: &[u8]) -> Result<(Vec<u8>, u32, u32)> {
    let mut reader = image::ImageReader::new(Cursor::new(data)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(20_000);
    limits.max_image_height = Some(20_000);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader.decode().context("无法解码候选图片")?;
    let dimensions = (decoded.width(), decoded.height());
    let thumbnail = decoded.thumbnail(THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT);
    let mut encoded = Cursor::new(Vec::new());
    thumbnail.write_to(&mut encoded, image::ImageFormat::Png)?;
    Ok((encoded.into_inner(), dimensions.0, dimensions.1))
}

fn office_metadata(data: &[u8]) -> Result<(Option<String>, Vec<String>)> {
    let mut archive = zip::ZipArchive::new(Cursor::new(data))?;
    let entry = match archive.by_name("docProps/core.xml") {
        Ok(entry) => entry,
        Err(zip::result::ZipError::FileNotFound) => return Ok((None, Vec::new())),
        Err(error) => return Err(error.into()),
    };
    if entry.size() > MAX_METADATA_BYTES {
        bail!("文档元数据过大");
    }
    let mut xml = Vec::new();
    Read::by_ref(&mut Read::take(entry, MAX_METADATA_BYTES + 1)).read_to_end(&mut xml)?;
    if xml.len() as u64 > MAX_METADATA_BYTES {
        bail!("文档元数据超过读取上限");
    }
    let title = xml_element(&xml, b"dc:title");
    let mut details = Vec::new();
    if let Some(created) = xml_element(&xml, b"dcterms:created") {
        details.push(format!("创建时间 {created}"));
    }
    if let Some(modified) = xml_element(&xml, b"dcterms:modified") {
        details.push(format!("修改时间 {modified}"));
    }
    Ok((title, details))
}

/// Office core.xml 是可选辅助信息；仅接受简单文本节点，失败时保持无标题。
fn xml_element(xml: &[u8], tag: &[u8]) -> Option<String> {
    let mut opening = vec![b'<'];
    opening.extend_from_slice(tag);
    let start = memmem::find(xml, &opening)?;
    let after_name = start + opening.len();
    if !xml
        .get(after_name)
        .is_some_and(|byte| *byte == b'>' || byte.is_ascii_whitespace())
    {
        return None;
    }
    let open_end = start + memmem::find(&xml[start..], b">")? + 1;
    let mut closing = b"</".to_vec();
    closing.extend_from_slice(tag);
    closing.push(b'>');
    let end = open_end + memmem::find(&xml[open_end..], &closing)?;
    let text = std::str::from_utf8(xml.get(open_end..end)?).ok()?;
    let decoded = text
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&");
    let cleaned = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.chars().take(200).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn candidate(kind: FileKind) -> DeviceCandidate {
        DeviceCandidate {
            id: "candidate_000001".into(),
            kind,
            size: 0,
            source_offset: 0,
        }
    }

    #[test]
    fn creates_bounded_png_thumbnail_and_reports_dimensions() {
        let image = image::DynamicImage::new_rgba8(640, 360);
        let mut encoded = Cursor::new(Vec::new());
        image
            .write_to(&mut encoded, image::ImageFormat::Png)
            .expect("PNG fixture");

        let preview = inspect_candidate(&candidate(FileKind::Png), &encoded.into_inner());
        let thumbnail = preview.thumbnail_png.expect("thumbnail");
        let decoded = image::load_from_memory(&thumbnail).expect("thumbnail PNG");

        assert!(decoded.width() <= THUMBNAIL_WIDTH);
        assert!(decoded.height() <= THUMBNAIL_HEIGHT);
        assert_eq!(preview.details, ["图片尺寸 640 × 360"]);
        assert!(preview.error.is_none());
    }

    #[test]
    fn invalid_image_has_nonfatal_preview_error() {
        let preview = inspect_candidate(&candidate(FileKind::Jpeg), b"broken jpeg");

        assert!(preview.thumbnail_png.is_none());
        assert!(preview.error.is_some());
    }

    #[test]
    fn reads_office_title_and_timestamps_without_using_original_file_name() {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        archive
            .start_file(
                "docProps/core.xml",
                zip::write::SimpleFileOptions::default(),
            )
            .expect("metadata entry");
        archive
            .write_all(
                r#"<cp:coreProperties xmlns:dc="dc" xmlns:dcterms="dcterms"><dc:title>季度 &amp; 恢复报告</dc:title><dcterms:created>2026-10-31T08:00:00Z</dcterms:created></cp:coreProperties>"#
                    .as_bytes(),
            )
            .expect("metadata contents");
        let data = archive.finish().expect("ZIP fixture").into_inner();

        let preview = inspect_candidate(&candidate(FileKind::Docx), &data);

        assert_eq!(preview.title.as_deref(), Some("季度 & 恢复报告"));
        assert_eq!(preview.details, ["创建时间 2026-10-31T08:00:00Z"]);
        assert!(preview.thumbnail_png.is_none());
    }

    #[test]
    fn xml_element_rejects_partial_tag_names() {
        assert_eq!(
            xml_element(b"<dc:titleExtra>wrong</dc:titleExtra>", b"dc:title"),
            None
        );
    }
}
