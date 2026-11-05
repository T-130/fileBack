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

use crate::{
    recovery::{FileKind, RecoveredFile, ScanOptions, ScanReport},
    system::ensure_destination_not_on_disk,
};
use anyhow::{Context, Result, bail};
use memchr::memmem;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

pub use crate::recovery::{DeviceCandidate, DeviceScanReport};

use crate::recovery::format;
const SCAN_BLOCK_SIZE: usize = 8 * 1024 * 1024;
const CARVE_BLOCK_SIZE: usize = 1024 * 1024;

/// 扫描过程的外部控制指令，由 GUI 通过标志文件下发。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanControl {
    Continue,
    Pause,
    Cancel,
}

/// 扫描回调：进度上报 + 暂停/取消控制 + 候选实时上报。多线程共享，要求 Sync。
#[derive(Clone, Copy, Default)]
pub struct ScanHooks<'a> {
    pub progress: Option<&'a (dyn Fn(u64) + Sync)>,
    pub control: Option<&'a (dyn Fn() -> ScanControl + Sync)>,
    pub candidate: Option<&'a (dyn Fn(&DeviceCandidate) + Sync)>,
}

#[cfg(target_os = "macos")]
pub fn scan_device_index(
    source: &Path,
    source_size: u64,
    options: ScanOptions,
    hooks: ScanHooks,
) -> Result<DeviceScanReport> {
    let file = open_raw_device(source, source_size)?;
    scan_file_ranges(file, source, source_size, options, hooks)
}

#[cfg(not(target_os = "macos"))]
pub fn scan_device_index(
    _source: &Path,
    _source_size: u64,
    _options: ScanOptions,
    _hooks: ScanHooks,
) -> Result<DeviceScanReport> {
    bail!("本地物理磁盘扫描目前仅支持 macOS")
}

/// 通过同一只读原始设备句柄补充 APFS 来源证据，不改变任何候选范围。
#[cfg(target_os = "macos")]
pub fn analyze_device_candidates(
    source: &Path,
    source_size: u64,
    candidates: &[DeviceCandidate],
) -> Result<crate::recovery::SourceAnalysis> {
    raw_device_identifier(source)?;
    let file = open_raw_device(source, source_size)?;
    let reader = RawDeviceReader::new(file, source_size);
    Ok(crate::filesystem::analyze_disk(
        reader,
        source_size,
        candidates,
    ))
}

#[cfg(not(target_os = "macos"))]
pub fn analyze_device_candidates(
    _source: &Path,
    _source_size: u64,
    _candidates: &[DeviceCandidate],
) -> Result<crate::recovery::SourceAnalysis> {
    bail!("本地物理磁盘来源分析目前仅支持 macOS")
}

/// apfs-core 使用 Seek + Read；这里沿用扫描器的 4096 字节对齐原始盘读取。
#[cfg(target_os = "macos")]
struct RawDeviceReader {
    file: File,
    source_size: u64,
    position: u64,
}

#[cfg(target_os = "macos")]
impl RawDeviceReader {
    fn new(file: File, source_size: u64) -> Self {
        Self {
            file,
            source_size,
            position: 0,
        }
    }
}

#[cfg(target_os = "macos")]
impl std::io::Read for RawDeviceReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        use anyhow::Context as _;
        if buffer.is_empty() || self.position >= self.source_size {
            return Ok(0);
        }
        let remaining = self.source_size - self.position;
        let length = usize::try_from(remaining)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let read = read_at(&self.file, &mut buffer[..length], self.position)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        self.position = self
            .position
            .checked_add(read as u64)
            .context("原始盘读取位置溢出")
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(read)
    }
}

#[cfg(target_os = "macos")]
impl std::io::Seek for RawDeviceReader {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        let next = match position {
            std::io::SeekFrom::Start(offset) => i128::from(offset),
            std::io::SeekFrom::Current(delta) => i128::from(self.position) + i128::from(delta),
            std::io::SeekFrom::End(delta) => i128::from(self.source_size) + i128::from(delta),
        };
        if next < 0 || next > i128::from(self.source_size) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "不能越过原始磁盘边界",
            ));
        }
        self.position = next as u64;
        Ok(self.position)
    }
}

#[cfg(target_os = "macos")]
pub fn preview_device_candidates(
    source: &Path,
    source_size: u64,
    candidates: &[DeviceCandidate],
) -> Result<Vec<crate::recovery::CandidatePreview>> {
    if candidates.len() > 10 {
        bail!("每次最多预览 10 个候选文件");
    }
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    raw_device_identifier(source)?;
    let file = open_raw_device(source, source_size)?;
    preview_candidate_ranges(&file, source_size, candidates)
}

#[cfg(target_os = "macos")]
fn preview_candidate_ranges(
    file: &File,
    source_size: u64,
    candidates: &[DeviceCandidate],
) -> Result<Vec<crate::recovery::CandidatePreview>> {
    let mut previews = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let end = candidate
            .source_offset
            .checked_add(candidate.size)
            .context("候选预览偏移无效")?;
        if candidate.size == 0 || end > source_size {
            bail!("候选文件 {} 超出有效预览范围", candidate.id);
        }
        if !matches!(
            candidate.kind,
            FileKind::Jpeg | FileKind::Png | FileKind::Docx | FileKind::Xlsx | FileKind::Pptx
        ) {
            previews.push(crate::recovery::inspect_candidate(candidate, &[]));
            continue;
        }
        if candidate.size > crate::recovery::preview::MAX_PREVIEW_SOURCE_BYTES {
            previews.push(crate::recovery::preview::unavailable_preview(
                candidate,
                "文件超过 64 MiB 预览上限，仍可正常恢复",
            ));
            continue;
        }
        let length = usize::try_from(candidate.size).context("候选预览大小无效")?;
        let mut data = Vec::new();
        data.try_reserve_exact(length)
            .context("无法分配候选预览内存")?;
        data.resize(length, 0);
        read_exact_at(file, &mut data, candidate.source_offset)?;
        previews.push(crate::recovery::inspect_candidate(candidate, &data));
    }
    Ok(previews)
}

#[cfg(not(target_os = "macos"))]
pub fn preview_device_candidates(
    _source: &Path,
    _source_size: u64,
    _candidates: &[DeviceCandidate],
) -> Result<Vec<crate::recovery::CandidatePreview>> {
    bail!("本地物理磁盘预览目前仅支持 macOS")
}

#[cfg(target_os = "macos")]
pub fn recover_device_candidates(
    source: &Path,
    source_size: u64,
    candidates: &[DeviceCandidate],
    output_dir: &Path,
    options: ScanOptions,
) -> Result<ScanReport> {
    if candidates.is_empty() {
        bail!("请先选择要恢复的候选文件");
    }
    let identifier = raw_device_identifier(source)?;
    if options.require_distinct_device {
        ensure_destination_not_on_disk(&identifier, output_dir)?;
    }
    prepare_output_directory(output_dir)?;
    let file = open_raw_device(source, source_size)?;
    let mut files = Vec::new();

    for candidate in candidates {
        let end = candidate
            .source_offset
            .checked_add(candidate.size)
            .context("候选文件偏移无效")?;
        if candidate.size == 0 || end > source_size || candidate.size > options.max_file_size as u64
        {
            bail!("候选文件 {} 超出有效范围", candidate.id);
        }
        let output_path =
            crate::recovery::output_file_path(output_dir, candidate.kind, files.len() + 1)?;
        write_range(&file, candidate.source_offset, end, &output_path)?;
        files.push(RecoveredFile {
            path: output_path,
            kind: candidate.kind,
            size: candidate.size,
            source_offset: candidate.source_offset,
        });
    }

    Ok(ScanReport {
        source: source.to_path_buf(),
        output_dir: output_dir.to_path_buf(),
        scanned_bytes: source_size,
        files,
        incomplete_candidates: 0,
        oversized_candidates: 0,
    })
}

#[cfg(not(target_os = "macos"))]
pub fn recover_device_candidates(
    _source: &Path,
    _source_size: u64,
    _candidates: &[DeviceCandidate],
    _output_dir: &Path,
    _options: ScanOptions,
) -> Result<ScanReport> {
    bail!("本地物理磁盘恢复目前仅支持 macOS")
}

#[cfg(target_os = "macos")]
fn open_raw_device(source: &Path, source_size: u64) -> Result<File> {
    use std::os::unix::fs::FileTypeExt;

    if source_size == 0 {
        bail!("源设备大小无效");
    }
    raw_device_identifier(source)?;
    if !fs::metadata(source)
        .context("无法读取源设备信息")?
        .file_type()
        .is_char_device()
    {
        bail!("源路径不是原始字符设备");
    }
    match OpenOptions::new().read(true).open(source) {
        Ok(file) => Ok(file),
        Err(error) if error.raw_os_error() == Some(libc::EPERM) => Err(anyhow::Error::new(error)
            .context("macOS 拒绝原始磁盘访问：请在“完全磁盘访问权限”中同时启用 FileBack 和 dev.fileback.FileBack.helper，并允许 FileBack 访问可移除卷宗，再重新打开 FileBack。管理员密码不能替代这些隐私授权；若助手未列出，请先开始一次扫描以安装助手")),
        Err(error) => Err(error).context("无法只读打开源设备，请完成管理员授权"),
    }
}

#[cfg(target_os = "macos")]
fn raw_device_identifier(source: &Path) -> Result<String> {
    source
        .to_str()
        .and_then(|path| path.strip_prefix("/dev/rdisk"))
        .filter(|suffix| !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()))
        .map(|suffix| format!("disk{suffix}"))
        .context("源设备必须是 /dev/rdiskN")
}

const CHUNK_SIZE: u64 = 64 * 1024 * 1024;

/// 多线程并行扫描：磁盘划分为 64MB 分块，按线程轮转分配；
/// 各线程独立完成读盘 + 文件头识别 + 雕刻，最后全局合并去重。
fn scan_file_ranges(
    file: File,
    source: &Path,
    source_size: u64,
    options: ScanOptions,
    hooks: ScanHooks,
) -> Result<DeviceScanReport> {
    if source_size == 0 {
        bail!("源设备大小无效");
    }
    // 线程数 = 全部核心数：扫描是磁盘密集型任务，可随时暂停释放资源
    let thread_count = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(4)
        .clamp(1, 16);
    let scanned = AtomicU64::new(0);
    let file = &file;
    let hooks = &hooks;
    let scanned_ref = &scanned;

    let chunk_results: Vec<Result<ChunkScan>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..thread_count)
            .map(|index| {
                scope.spawn(move || {
                    scan_chunks(
                        file,
                        source_size,
                        options,
                        hooks,
                        scanned_ref,
                        index as u64,
                        thread_count as u64,
                    )
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or_else(|_| bail!("扫描线程异常终止")))
            .collect()
    });

    let mut report = DeviceScanReport {
        source: source.to_path_buf(),
        source_size,
        source_fingerprint: String::new(),
        candidates: Vec::new(),
        incomplete_candidates: 0,
        oversized_candidates: 0,
        canceled: false,
        source_analysis: crate::recovery::SourceAnalysis::default(),
    };
    for result in results_in_order(chunk_results)? {
        report.candidates.extend(result.candidates);
        report.incomplete_candidates += result.incomplete_candidates;
        report.oversized_candidates += result.oversized_candidates;
        report.canceled |= result.canceled;
    }

    // 全局去重：候选按偏移排序，丢弃落在前一个文件范围内的重叠项
    report
        .candidates
        .sort_by_key(|candidate| candidate.source_offset);
    let mut merged: Vec<DeviceCandidate> = Vec::with_capacity(report.candidates.len());
    let mut covered_until = 0_u64;
    for candidate in report.candidates {
        if candidate.source_offset < covered_until {
            continue;
        }
        covered_until = candidate.source_offset + candidate.size;
        merged.push(candidate);
    }
    for (index, candidate) in merged.iter_mut().enumerate() {
        candidate.id = format!("candidate_{:06}", index + 1);
    }
    report.candidates = merged;
    Ok(report)
}

fn results_in_order(results: Vec<Result<ChunkScan>>) -> Result<Vec<ChunkScan>> {
    results.into_iter().collect()
}

struct ChunkScan {
    candidates: Vec<DeviceCandidate>,
    incomplete_candidates: usize,
    oversized_candidates: usize,
    canceled: bool,
}

fn scan_chunks(
    file: &File,
    source_size: u64,
    options: ScanOptions,
    hooks: &ScanHooks,
    scanned: &AtomicU64,
    thread_index: u64,
    thread_count: u64,
) -> Result<ChunkScan> {
    let mut result = ChunkScan {
        candidates: Vec::new(),
        incomplete_candidates: 0,
        oversized_candidates: 0,
        canceled: false,
    };
    let mut chunk_index = thread_index;

    while chunk_index * CHUNK_SIZE < source_size {
        let chunk_start = chunk_index * CHUNK_SIZE;
        let chunk_end = (chunk_start + CHUNK_SIZE).min(source_size);
        let mut offset = chunk_start;
        let header_overlap = format::FILE_KINDS
            .iter()
            .map(|kind| format::header_context_len(*kind) - 1)
            .max()
            .unwrap_or(0);
        // 带上前一分块的末尾字节，让跨 64 MiB 边界的文件头由当前分块识别。
        let prefix_len = usize::try_from(chunk_start)
            .unwrap_or(usize::MAX)
            .min(header_overlap);
        let mut carry = vec![0_u8; prefix_len];
        if prefix_len > 0 {
            read_exact_at(file, &mut carry, chunk_start - prefix_len as u64)?;
        }
        let mut skip_until = 0_u64;

        while offset < chunk_end {
            if wait_for_control(hooks) == ScanControl::Cancel {
                result.canceled = true;
                return Ok(result);
            }

            let read_size = (chunk_end - offset).min(SCAN_BLOCK_SIZE as u64) as usize;
            let carry_size = carry.len();
            let mut buffer = carry;
            buffer.resize(carry_size + read_size, 0);
            let read = read_at(file, &mut buffer[carry_size..], offset)?;
            if read == 0 {
                bail!("读取源设备时提前到达末尾，扫描结果不完整");
            }
            buffer.truncate(carry_size + read);
            let base = offset - carry_size as u64;
            let mut search_cursor = 0;

            while search_cursor < buffer.len() {
                let Some((position, next, kind)) = format::find_next_header(&buffer, search_cursor)
                else {
                    break;
                };
                let start = base + position as u64;
                search_cursor = next;
                // 完全位于 overlap 中的文件头已在上一轮处理；跨边界文件头必须保留。
                let header_end = start + format::header_context_len(kind) as u64;
                if header_end <= offset || start < skip_until || start >= chunk_end {
                    continue;
                }

                let end = match find_file_end(
                    file,
                    start,
                    kind,
                    source_size,
                    options.max_file_size,
                    hooks,
                )? {
                    EndSearch::Found(end) => end,
                    EndSearch::Missing => {
                        if start.saturating_add(options.max_file_size as u64) < source_size {
                            result.oversized_candidates += 1;
                        } else {
                            result.incomplete_candidates += 1;
                        }
                        continue;
                    }
                    EndSearch::Canceled => {
                        result.canceled = true;
                        return Ok(result);
                    }
                };
                let length = end - start;
                if length > options.max_file_size as u64 {
                    result.oversized_candidates += 1;
                    skip_until = end;
                    continue;
                }
                if kind == FileKind::Jpeg {
                    // 只读取有限的头部；超大元数据导致无法判断时保守保留候选。
                    let header_length = length.min(64 * 1024) as usize;
                    let mut header = vec![0_u8; header_length];
                    read_exact_at(file, &mut header, start)?;
                    match format::jpeg_header_is_valid(&header) {
                        Some(true) => {}
                        None if header_length as u64 != length => {}
                        Some(false) | None => continue,
                    }
                }
                let kind = if kind == FileKind::Zip {
                    classify_zip_candidate(file, start, end)?
                } else {
                    kind
                };

                let candidate = DeviceCandidate {
                    id: String::new(), // 合并阶段统一编号
                    kind,
                    size: length,
                    source_offset: start,
                };
                if let Some(callback) = hooks.candidate {
                    callback(&candidate);
                }
                result.candidates.push(candidate);
                skip_until = end;
            }

            let overlap = header_overlap.min(buffer.len());
            carry = buffer[buffer.len() - overlap..].to_vec();
            offset += read as u64;
            let done = scanned.fetch_add(read as u64, Ordering::Relaxed) + read as u64;
            if let Some(progress) = hooks.progress {
                progress(done);
            }
        }

        chunk_index += thread_count;
    }

    Ok(result)
}

fn wait_for_control(hooks: &ScanHooks) -> ScanControl {
    let Some(control) = hooks.control else {
        return ScanControl::Continue;
    };
    loop {
        match control() {
            ScanControl::Continue => return ScanControl::Continue,
            ScanControl::Pause => std::thread::sleep(std::time::Duration::from_millis(100)),
            ScanControl::Cancel => return ScanControl::Cancel,
        }
    }
}

enum EndSearch {
    Found(u64),
    Missing,
    Canceled,
}

fn find_file_end(
    file: &File,
    start: u64,
    kind: FileKind,
    source_size: u64,
    max_file_size: usize,
    hooks: &ScanHooks,
) -> Result<EndSearch> {
    let limit = source_size.min(start.saturating_add(max_file_size as u64));
    match kind {
        FileKind::Avi | FileKind::Wav => {
            let mut header = [0_u8; 12];
            if read_exact_at(file, &mut header, start).is_err() {
                return Ok(EndSearch::Missing);
            }
            let Some(length) = format::riff_file_len(&header, kind) else {
                return Ok(EndSearch::Missing);
            };
            let Some(end) = start.checked_add(length) else {
                return Ok(EndSearch::Missing);
            };
            return Ok(if end <= limit {
                EndSearch::Found(end)
            } else {
                EndSearch::Missing
            });
        }
        FileKind::Mp4 | FileKind::Mov | FileKind::M4a => {
            return find_iso_media_end(file, start, limit, hooks);
        }
        _ => {}
    }
    let search_start = start
        .checked_add(kind.header().len() as u64)
        .context("候选文件偏移溢出")?;
    let Some(footer) = kind.footer() else {
        bail!("文件格式不支持边界扫描");
    };
    let mut offset = search_start;
    let mut carry = Vec::new();

    while offset < limit {
        if wait_for_control(hooks) == ScanControl::Cancel {
            return Ok(EndSearch::Canceled);
        }
        let read_size = (limit - offset).min(CARVE_BLOCK_SIZE as u64) as usize;
        let carry_size = carry.len();
        let mut buffer = carry;
        buffer.resize(carry_size + read_size, 0);
        let read = read_at(file, &mut buffer[carry_size..], offset)?;
        if read == 0 {
            return Ok(EndSearch::Missing);
        }
        buffer.truncate(carry_size + read);
        let buffer_start = offset - carry_size as u64;
        let mut search_cursor = 0;
        while let Some(relative_position) = memmem::find(&buffer[search_cursor..], footer) {
            let position = search_cursor + relative_position;
            search_cursor = position + 1;
            let footer_start = buffer_start + position as u64;
            let end = if kind.is_zip_container() {
                // EOCD 固定部分有 22 字节。截断记录只是不完整候选，不能中断整盘扫描。
                let Some(comment_length_bytes) = buffer.get(position + 20..position + 22) else {
                    continue;
                };
                let comment_length = u16::from_le_bytes(comment_length_bytes.try_into()?) as u64;
                let Some(end) = footer_start
                    .checked_add(22)
                    .and_then(|end| end.checked_add(comment_length))
                else {
                    continue;
                };
                if end > limit {
                    // 注释长度无效时继续找后面的 EOCD，避免伪造签名遮蔽有效记录。
                    continue;
                }
                end
            } else {
                footer_start
                    .checked_add(footer.len() as u64)
                    .context("候选文件结束位置溢出")?
            };
            return Ok(EndSearch::Found(end));
        }

        let overlap = (footer.len() - 1).min(buffer.len());
        carry = buffer[buffer.len() - overlap..].to_vec();
        offset += read as u64;
    }
    Ok(EndSearch::Missing)
}

fn find_iso_media_end(file: &File, start: u64, limit: u64, hooks: &ScanHooks) -> Result<EndSearch> {
    let mut offset = start;
    let mut first = true;
    let mut has_media = false;
    let mut has_metadata = false;
    while offset < limit {
        if wait_for_control(hooks) == ScanControl::Cancel {
            return Ok(EndSearch::Canceled);
        }
        let read_size = (limit - offset).min(16) as usize;
        let mut header = vec![0_u8; read_size];
        if read_exact_at(file, &mut header, offset).is_err() {
            return Ok(EndSearch::Missing);
        }
        let Some(atom) = format::iso_atom(&header) else {
            return Ok(EndSearch::Missing);
        };
        if (first && atom.kind != *b"ftyp") || (!first && atom.kind == *b"ftyp") {
            return Ok(EndSearch::Missing);
        }
        first = false;
        has_media |= atom.kind == *b"mdat";
        has_metadata |= atom.kind == *b"moov";
        let Some(end) = offset.checked_add(atom.size) else {
            return Ok(EndSearch::Missing);
        };
        if end > limit {
            return Ok(EndSearch::Missing);
        }
        offset = end;
        if has_media && has_metadata {
            return Ok(EndSearch::Found(end));
        }
    }
    Ok(EndSearch::Missing)
}

fn classify_zip_candidate(file: &File, start: u64, end: u64) -> Result<FileKind> {
    const MAX_DIRECTORY_SIZE: usize = 16 * 1024 * 1024;
    let length = end - start;
    let tail_size = length.min(22 + u16::MAX as u64) as usize;
    let mut tail = vec![0_u8; tail_size];
    read_exact_at(file, &mut tail, end - tail_size as u64)?;
    let mut read_error = None;
    let kind = format::classify_zip_end_record(&tail, |offset, size| {
        if size > MAX_DIRECTORY_SIZE || offset.checked_add(size)? as u64 > length {
            return None;
        }
        let mut directory = vec![0_u8; size];
        let absolute = start.checked_add(offset as u64)?;
        if let Err(error) = read_exact_at(file, &mut directory, absolute) {
            read_error = Some(error);
            return None;
        }
        Some(directory)
    });
    if let Some(error) = read_error {
        return Err(error);
    }
    Ok(kind)
}

fn write_range(file: &File, start: u64, end: u64, output_path: &Path) -> Result<()> {
    let parent = output_path.parent().context("恢复文件路径无效")?;
    let mut output = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("无法创建临时恢复文件：{}", output_path.display()))?;
    let mut offset = start;
    let mut buffer = vec![0_u8; CARVE_BLOCK_SIZE];
    while offset < end {
        let read_size = (end - offset).min(buffer.len() as u64) as usize;
        read_exact_at(file, &mut buffer[..read_size], offset)?;
        output
            .write_all(&buffer[..read_size])
            .with_context(|| format!("无法写入恢复文件：{}", output_path.display()))?;
        offset += read_size as u64;
    }
    output
        .as_file()
        .sync_all()
        .with_context(|| format!("无法同步恢复文件：{}", output_path.display()))?;
    // 发布失败时临时文件自动清理，且不覆盖已有文件。
    crate::recovery::persist_recovered_file(output, output_path)?;
    Ok(())
}

#[cfg(unix)]
fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> Result<usize> {
    use std::os::unix::fs::FileExt;
    // 原始字符设备要求按物理扇区对齐读取；Apple SSD 物理扇区为 4096 字节，
    // 文件头可能出现在任意偏移，未对齐的偏移或长度都会被内核拒绝（EINVAL），
    // 这里统一下压到 4096 边界再裁剪。
    const ALIGN: u64 = 4096;
    let aligned_start = offset / ALIGN * ALIGN;
    let skip = (offset - aligned_start) as usize;
    if skip == 0 && (buffer.len() as u64).is_multiple_of(ALIGN) {
        return file
            .read_at(buffer, offset)
            .with_context(|| format!("只读扫描失败，偏移：{offset}"));
    }
    let padded_len = (skip + buffer.len()).div_ceil(ALIGN as usize) * ALIGN as usize;
    let mut padded = vec![0_u8; padded_len];
    let read = file
        .read_at(&mut padded, aligned_start)
        .with_context(|| format!("只读扫描失败，偏移：{offset}"))?;
    if read <= skip {
        return Ok(0);
    }
    let copied = (read - skip).min(buffer.len());
    buffer[..copied].copy_from_slice(&padded[skip..skip + copied]);
    Ok(copied)
}

#[cfg(unix)]
fn read_exact_at(file: &File, mut buffer: &mut [u8], mut offset: u64) -> Result<()> {
    while !buffer.is_empty() {
        let read = read_at(file, buffer, offset)?;
        if read == 0 {
            bail!("读取源设备时提前到达末尾");
        }
        offset += read as u64;
        buffer = &mut buffer[read..];
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

#[cfg(test)]
mod tests;
