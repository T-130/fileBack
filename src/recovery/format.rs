//! 文件格式签名、ZIP 内部结构及可验证的容器边界。

use super::FileKind;
use memchr::memmem;

pub(crate) const FILE_KINDS: [FileKind; 7] = [
    FileKind::Jpeg,
    FileKind::Png,
    FileKind::Pdf,
    FileKind::Zip,
    FileKind::Mp4,
    FileKind::Avi,
    FileKind::Wav,
];

/// 返回签名、实际起始位置及签名后的搜索位置，避免带前缀签名导致扫描回退。
pub(crate) fn find_next_header(data: &[u8], cursor: usize) -> Option<(usize, usize, FileKind)> {
    FILE_KINDS
        .iter()
        .filter_map(|kind| {
            let mut search = cursor;
            while search < data.len() {
                let position = search + memmem::find(&data[search..], kind.header())?;
                if let Some((start, recognized)) = recognized_header(*kind, data, position) {
                    return Some((start, position + kind.header().len(), recognized));
                }
                // 无效签名不能遮蔽同一读取块内后面的真实文件头。
                search = position + 1;
            }
            None
        })
        .min_by_key(|(position, _, _)| *position)
}

pub(crate) fn header_context_len(kind: FileKind) -> usize {
    match kind {
        FileKind::Mp4 | FileKind::Mov | FileKind::M4a | FileKind::Avi | FileKind::Wav => 12,
        _ => kind.header().len(),
    }
}

/// 校验 JPEG 在压缩数据前的基本结构，不要求完整解码。
/// `None` 表示当前字节不足；物理盘读取部分头部时应保守保留该候选。
pub(crate) fn jpeg_header_is_valid(data: &[u8]) -> Option<bool> {
    if data.len() < 3 {
        return None;
    }
    if !data.starts_with(b"\xff\xd8\xff") {
        return Some(false);
    }

    let mut offset = 2_usize;
    let mut has_frame = false;
    loop {
        let &byte = data.get(offset)?;
        if byte != 0xff {
            return Some(false);
        }
        while data.get(offset) == Some(&0xff) {
            offset += 1;
        }
        let &marker = data.get(offset)?;
        offset += 1;

        match marker {
            0x00 | 0xd8 | 0xd9 => return Some(false),
            0x01 | 0xd0..=0xd7 => continue,
            _ => {}
        }

        let length_bytes = data.get(offset..offset + 2)?;
        let length = u16::from_be_bytes(length_bytes.try_into().ok()?) as usize;
        if length < 2 {
            return Some(false);
        }
        let Some(end) = offset.checked_add(length) else {
            return Some(false);
        };
        if end > data.len() {
            return None;
        }

        if marker == 0xda {
            if length < 6 {
                return Some(false);
            }
            let components = usize::from(data[offset + 2]);
            if components == 0 || length != 6 + 2 * components {
                return Some(false);
            }
            return Some(has_frame);
        }
        if matches!(
            marker,
            0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf
        ) {
            if length < 8 {
                return Some(false);
            }
            let precision = data[offset + 2];
            let height = u16::from_be_bytes(data.get(offset + 3..offset + 5)?.try_into().ok()?);
            let width = u16::from_be_bytes(data.get(offset + 5..offset + 7)?.try_into().ok()?);
            let components = usize::from(data[offset + 7]);
            if precision == 0
                || width == 0
                || height == 0
                || components == 0
                || length != 8 + 3 * components
            {
                return Some(false);
            }
            has_frame = true;
        }
        offset = end;
    }
}

fn recognized_header(kind: FileKind, data: &[u8], position: usize) -> Option<(usize, FileKind)> {
    match kind {
        FileKind::Mp4 => {
            let start = position.checked_sub(4)?;
            let size = u32::from_be_bytes(data.get(start..position)?.try_into().ok()?);
            if size < 16 {
                return None;
            }
            let kind = classify_iso_brand(data, position)?;
            Some((start, kind))
        }
        FileKind::Avi | FileKind::Wav => {
            let start = position.checked_sub(8)?;
            riff_file_len(data.get(start..start + 12)?, kind)?;
            Some((start, kind))
        }
        _ => Some((position, kind)),
    }
}

fn classify_iso_brand(data: &[u8], ftyp_position: usize) -> Option<FileKind> {
    match data.get(ftyp_position + 4..ftyp_position + 8)? {
        b"qt  " => Some(FileKind::Mov),
        b"M4A " => Some(FileKind::M4a),
        b"isom" | b"iso2" | b"iso5" | b"iso6" | b"mp41" | b"mp42" | b"avc1" | b"M4V " | b"M4VH"
        | b"M4VP" | b"dash" => Some(FileKind::Mp4),
        _ => None,
    }
}

/// 从候选末尾的 EOCD 记录定位中央目录；ZIP64 或不一致时保留为普通 ZIP。
fn zip_directory_range(end_record: &[u8]) -> Option<(usize, usize)> {
    let position = memmem::rfind(end_record, b"PK\x05\x06")?;
    let record = end_record.get(position..)?;
    if record.len() < 22
        || read_u16(record, 4)? != 0
        || read_u16(record, 6)? != 0
        || read_u16(record, 8)? != read_u16(record, 10)?
    {
        return None;
    }
    let entries = read_u16(record, 10)?;
    let comment = read_u16(record, 20)? as usize;
    if entries == 0
        || entries == u16::MAX
        || position.checked_add(22)?.checked_add(comment)? != end_record.len()
    {
        return None;
    }
    let size = read_u32(record, 12)? as usize;
    let offset = read_u32(record, 16)? as usize;
    if size == 0 || offset == u32::MAX as usize || size == u32::MAX as usize {
        return None;
    }
    Some((offset, size))
}

pub(crate) fn classify_zip(data: &[u8]) -> FileKind {
    let Some((offset, size)) = zip_directory_range(data) else {
        return FileKind::Zip;
    };
    let Some(end) = offset.checked_add(size) else {
        return FileKind::Zip;
    };
    data.get(offset..end)
        .and_then(classify_zip_directory)
        .unwrap_or(FileKind::Zip)
}

pub(crate) fn classify_zip_end_record(
    end_record: &[u8],
    directory: impl FnOnce(usize, usize) -> Option<Vec<u8>>,
) -> FileKind {
    zip_directory_range(end_record)
        .and_then(|(offset, size)| directory(offset, size))
        .and_then(|data| classify_zip_directory(&data))
        .unwrap_or(FileKind::Zip)
}

fn classify_zip_directory(mut data: &[u8]) -> Option<FileKind> {
    let mut content_types = false;
    let mut word = false;
    let mut excel = false;
    let mut powerpoint = false;
    while !data.is_empty() {
        if data.get(..4)? != b"PK\x01\x02" || data.len() < 46 {
            return None;
        }
        let name_length = read_u16(data, 28)? as usize;
        let extra_length = read_u16(data, 30)? as usize;
        let comment_length = read_u16(data, 32)? as usize;
        let name_end = 46_usize.checked_add(name_length)?;
        let total = name_end
            .checked_add(extra_length)?
            .checked_add(comment_length)?;
        let name = data.get(46..name_end)?;
        content_types |= name == b"[Content_Types].xml";
        word |= name == b"word/document.xml";
        excel |= name == b"xl/workbook.xml";
        powerpoint |= name == b"ppt/presentation.xml";
        data = data.get(total..)?;
    }
    if !content_types {
        return Some(FileKind::Zip);
    }
    Some(match (word, excel, powerpoint) {
        (true, false, false) => FileKind::Docx,
        (false, true, false) => FileKind::Xlsx,
        (false, false, true) => FileKind::Pptx,
        _ => FileKind::Zip,
    })
}

fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        data.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

pub(crate) fn riff_file_len(header: &[u8], kind: FileKind) -> Option<u64> {
    if header.len() < 12 || header.get(..4)? != b"RIFF" {
        return None;
    }
    let expected = match kind {
        FileKind::Avi => b"AVI ".as_slice(),
        FileKind::Wav => b"WAVE".as_slice(),
        _ => return None,
    };
    if header.get(8..12)? != expected {
        return None;
    }
    let length = u64::from(u32::from_le_bytes(header.get(4..8)?.try_into().ok()?)) + 8;
    (length >= 12).then_some(length)
}

pub(crate) struct IsoAtom {
    pub(crate) size: u64,
    pub(crate) kind: [u8; 4],
}

pub(crate) fn iso_atom(header: &[u8]) -> Option<IsoAtom> {
    let size = u32::from_be_bytes(header.get(..4)?.try_into().ok()?);
    let kind = header.get(4..8)?.try_into().ok()?;
    let size = match size {
        0 => return None,
        1 => {
            let size = u64::from_be_bytes(header.get(8..16)?.try_into().ok()?);
            if size < 16 {
                return None;
            }
            size
        }
        size if size < 8 => return None,
        size => u64::from(size),
    };
    Some(IsoAtom { size, kind })
}

pub(crate) fn iso_media_end(data: &[u8]) -> Option<usize> {
    classify_iso_brand(data, 4)?;
    let mut offset = 0_usize;
    let mut first = true;
    let mut has_media = false;
    let mut has_metadata = false;
    while let Some(header) = data.get(offset..) {
        let atom = iso_atom(header)?;
        if (first && atom.kind != *b"ftyp") || (!first && atom.kind == *b"ftyp") {
            return None;
        }
        first = false;
        has_media |= atom.kind == *b"mdat";
        has_metadata |= atom.kind == *b"moov";
        offset = offset.checked_add(usize::try_from(atom.size).ok()?)?;
        if offset > data.len() {
            return None;
        }
        if has_media && has_metadata {
            return Some(offset);
        }
    }
    None
}
