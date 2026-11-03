use anyhow::{Context, Result, bail};
use memchr::memmem;
use memmap2::MmapOptions;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

pub(crate) mod format;
mod model;
pub(crate) mod preview;
pub use model::{
    CandidatePreview, CandidateSource, CandidateSourceState, DeviceCandidate, DeviceScanReport,
    FileCategory, FileKind, RecoveredFile, ScanOptions, ScanReport, SourceAnalysis,
};
pub use preview::inspect_candidate;

pub fn scan_image(source: &Path, output_dir: &Path, options: ScanOptions) -> Result<ScanReport> {
    let source = source
        .canonicalize()
        .with_context(|| format!("无法访问源镜像：{}", source.display()))?;
    let source_metadata = fs::metadata(&source).context("无法读取源镜像信息")?;
    if !source_metadata.is_file() {
        bail!("第一版仅支持普通磁盘镜像文件，暂不支持直接扫描设备或目录");
    }
    if source_metadata.len() == 0 {
        bail!("源镜像为空");
    }

    if options.require_distinct_device {
        ensure_distinct_device(&source, output_dir)?;
    }
    prepare_output_directory(output_dir)?;

    let file = OpenOptions::new()
        .read(true)
        .open(&source)
        .context("无法以只读方式打开源镜像")?;
    // The mapping is read-only, and the file remains open for its lifetime.
    let data = unsafe { MmapOptions::new().map(&file) }.context("无法映射源镜像")?;

    let mut report = ScanReport {
        source,
        output_dir: output_dir.to_path_buf(),
        scanned_bytes: data.len() as u64,
        files: Vec::new(),
        incomplete_candidates: 0,
        oversized_candidates: 0,
    };
    let mut cursor = 0;

    while cursor < data.len() {
        let Some((start, next, kind)) = format::find_next_header(&data, cursor) else {
            break;
        };
        let Some(end) = find_file_end(&data, start, kind) else {
            report.incomplete_candidates += 1;
            // 损坏或误识别的候选不应阻断后续完整文件的恢复。
            cursor = next;
            continue;
        };
        let length = end - start;
        if length > options.max_file_size {
            report.oversized_candidates += 1;
            cursor = end;
            continue;
        }

        if kind == FileKind::Jpeg && format::jpeg_header_is_valid(&data[start..end]) != Some(true) {
            // 明确无效的完整候选，以及缺少图像头的候选，不再发布为 JPEG。
            cursor = next;
            continue;
        }
        let kind = if kind == FileKind::Zip {
            format::classify_zip(&data[start..end])
        } else {
            kind
        };
        let output_path = output_file_path(output_dir, kind, report.files.len() + 1)?;
        let parent = output_path.parent().context("恢复文件路径无效")?;
        let mut output = tempfile::NamedTempFile::new_in(parent)
            .with_context(|| format!("无法创建临时恢复文件：{}", output_path.display()))?;
        output
            .write_all(&data[start..end])
            .with_context(|| format!("无法写入恢复文件：{}", output_path.display()))?;
        output
            .as_file()
            .sync_all()
            .with_context(|| format!("无法同步恢复文件：{}", output_path.display()))?;
        persist_recovered_file(output, &output_path)?;

        report.files.push(RecoveredFile {
            path: output_path,
            kind,
            size: length as u64,
            source_offset: start as u64,
        });
        cursor = end;
    }

    Ok(report)
}

/// 优先使用禁止覆盖的原子重命名；ExFAT 不支持该操作时，先独占创建空占位文件。
/// 占位文件只能由本次操作创建，原子重命名仅替换它，不覆盖用户已有文件。
pub(crate) fn persist_recovered_file(
    output: tempfile::NamedTempFile,
    output_path: &Path,
) -> Result<()> {
    match output.persist_noclobber(output_path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.raw_os_error() == Some(libc::ENOTSUP) => {
            persist_with_exclusive_placeholder(error.file, output_path)
        }
        Err(error) => Err(anyhow::Error::new(error.error)),
    }
    .with_context(|| format!("无法发布恢复文件：{}", output_path.display()))
}

fn persist_with_exclusive_placeholder(
    output: tempfile::NamedTempFile,
    output_path: &Path,
) -> Result<()> {
    let placeholder = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output_path)
        .with_context(|| format!("无法独占创建恢复文件：{}", output_path.display()))?;
    drop(placeholder);
    if let Err(error) = fs::rename(output.path(), output_path) {
        return match fs::remove_file(output_path) {
            Ok(()) => Err(error).context("无法通过兼容方式发布恢复文件"),
            Err(cleanup) => {
                Err(error).context(format!("无法发布恢复文件，且清理空占位文件失败：{cleanup}"))
            }
        };
    }
    Ok(())
}

fn prepare_output_directory(output_dir: &Path) -> Result<()> {
    if output_dir.exists() {
        if !output_dir.is_dir() {
            bail!("恢复目标不是目录");
        }
        if fs::read_dir(output_dir)
            .context("无法读取恢复目录")?
            .next()
            .is_some()
        {
            bail!("恢复目录必须为空，以免覆盖或混淆已有文件");
        }
    } else {
        fs::create_dir_all(output_dir).context("无法创建恢复目录")?;
    }
    Ok(())
}

pub(crate) fn output_file_path(
    output_dir: &Path,
    kind: FileKind,
    index: usize,
) -> Result<std::path::PathBuf> {
    let directory = output_dir.join(kind.category().directory());
    match fs::create_dir(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if !fs::symlink_metadata(&directory)?.file_type().is_dir() {
                bail!("恢复分类目录不是普通目录");
            }
        }
        Err(error) => return Err(error).context("无法创建恢复分类目录"),
    }
    Ok(directory.join(format!("fileback_{index:06}.{}", kind.extension())))
}

#[cfg(unix)]
fn ensure_distinct_device(source: &Path, output_dir: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let source_device = fs::metadata(source)?.dev();
    let mut existing_ancestor = output_dir;
    while !existing_ancestor.exists() {
        existing_ancestor = match existing_ancestor.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
    }
    let output_device = fs::metadata(existing_ancestor)?.dev();
    if source_device == output_device {
        bail!("源镜像和恢复目录位于同一设备，请选择另一块磁盘作为恢复目标");
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_distinct_device(_source: &Path, _output_dir: &Path) -> Result<()> {
    Ok(())
}

fn find_file_end(data: &[u8], start: usize, kind: FileKind) -> Option<usize> {
    match kind {
        FileKind::Avi | FileKind::Wav => {
            let length = format::riff_file_len(data.get(start..start + 12)?, kind)?;
            let end = start.checked_add(usize::try_from(length).ok()?)?;
            return (end <= data.len()).then_some(end);
        }
        FileKind::Mp4 | FileKind::Mov | FileKind::M4a => {
            return start.checked_add(format::iso_media_end(data.get(start..)?)?);
        }
        _ => {}
    }
    let search_start = start.checked_add(kind.header().len())?;
    let footer = kind.footer()?;
    let footer_start = search_start + memmem::find(data.get(search_start..)?, footer)?;

    if kind.is_zip_container() {
        let comment_length_position = footer_start.checked_add(20)?;
        let comment_length_bytes: [u8; 2] = data
            .get(comment_length_position..comment_length_position + 2)?
            .try_into()
            .ok()?;
        let comment_length = u16::from_le_bytes(comment_length_bytes) as usize;
        let end = footer_start.checked_add(22 + comment_length)?;
        (end <= data.len()).then_some(end)
    } else {
        Some(footer_start + footer.len())
    }
}

#[cfg(test)]
mod tests;
