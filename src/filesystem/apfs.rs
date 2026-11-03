//! 基于 apfs-core 的容器、现存文件、历史快照与空间管理器只读分析。

use super::{
    ContainerEvidence, Location, RangeSet, ViewEvidence,
    gpt::{Partition, PartitionView},
};
use crate::recovery::DeviceCandidate;
use anyhow::{Context, Result, bail};
use apfs_core::{
    ApfsContainer,
    dir::{ROOT_DIR_INO_NUM, list_dir, load_inode},
    extent::{FileExtent, list_extents},
    snapshot::{list_snapshots, mount_snapshot},
    spaceman::free_block_runs,
    volume::ApfsVolume,
};
use std::{
    collections::{HashMap, HashSet},
    io::{Read, Seek, SeekFrom},
};

const MAX_DIRECTORY_DEPTH: usize = 128;
const MAX_PATH_BYTES: usize = 4096;
const MAX_CONTAINER_ENTRIES: usize = 1_000_000;
const MAX_CONTAINER_SNAPSHOTS: usize = 128;
const MAX_SNAPSHOTS_PER_VOLUME: usize = 32;
const MAX_EXTENTS_PER_FILE: usize = 1_000_000;
const S_IFMT: u16 = 0o170000;
const S_IFREG: u16 = 0o100000;
const S_IFDIR: u16 = 0o040000;

type ExactCandidates = HashMap<(u64, u64), Vec<String>>;

pub(crate) fn analyze_container<R: Read + Seek>(
    reader: &mut R,
    partition: &Partition,
    candidates: &[&DeviceCandidate],
) -> Result<ContainerEvidence> {
    let candidate_ranges = candidate_ranges(candidates);
    let exact_candidates = exact_candidates(candidates);
    let mut view = PartitionView::new(reader, partition);
    let mut container = ApfsContainer::open(&mut view).context("无法打开 APFS 容器")?;
    let block_size = container.superblock().block_size as usize;
    let container_size = u64::from(container.superblock().block_size)
        .checked_mul(container.superblock().block_count)
        .context("APFS 容器块范围溢出")?;
    if container_size == 0 || container_size > partition.len {
        bail!("APFS 容器块范围超出 GPT 分区");
    }
    let volume_addrs = container.volume_superblock_addrs();
    let spaceman = container.spaceman_paddr();
    let reader = container.into_reader();

    let mut warnings = Vec::new();
    let mut free = match spaceman {
        Some(spaceman) => match free_block_runs(reader, spaceman, block_size) {
            Ok(runs) => Some(free_ranges(
                runs,
                partition.start,
                block_size,
                container_size,
                &candidate_ranges,
            )?),
            Err(error) => {
                warning(&mut warnings, format!("APFS 空闲块分析不可用：{error}"));
                None
            }
        },
        None => {
            warning(&mut warnings, "APFS 空间管理器没有可用的物理地址".into());
            None
        }
    };

    let mut current = ViewEvidence::default();
    let mut snapshots = ViewEvidence::default();
    let mut budget = Budget::default();
    let volume_addrs = match volume_addrs {
        Ok(addrs) => addrs,
        Err(error) => {
            warning(&mut warnings, format!("无法枚举 APFS 卷宗：{error}"));
            Vec::new()
        }
    };

    for address in volume_addrs {
        let volume = match read_volume(reader, address, block_size, container_size) {
            Ok(volume) => volume,
            Err(error) => {
                warning(
                    &mut warnings,
                    format!("无法读取 APFS 卷宗超级块（块 {address}）：{error:#}"),
                );
                continue;
            }
        };
        let volume_name = volume.name().to_string();
        let walked = walk_volume(
            reader,
            &volume,
            None,
            partition.start,
            block_size,
            container_size,
            &candidate_ranges,
            &exact_candidates,
            &mut budget,
        );
        merge_walk(
            &mut current,
            walked,
            &mut warnings,
            &format!("卷宗 {volume_name}"),
        );

        let snapshot_list = match list_snapshots(reader, &volume, block_size) {
            Ok(snapshots) => snapshots,
            Err(error) => {
                warning(
                    &mut warnings,
                    format!("卷宗 {volume_name} 的快照列表不可用：{error}"),
                );
                continue;
            }
        };
        if snapshot_list.len() > MAX_SNAPSHOTS_PER_VOLUME {
            warning(
                &mut warnings,
                format!(
                    "卷宗 {volume_name} 有 {} 个快照，仅分析最新 {MAX_SNAPSHOTS_PER_VOLUME} 个",
                    snapshot_list.len()
                ),
            );
        }
        for snapshot in snapshot_list.iter().rev().take(MAX_SNAPSHOTS_PER_VOLUME) {
            if budget.snapshots_remaining == 0 {
                warning(
                    &mut warnings,
                    "容器快照分析达到 128 个上限，其余快照未检查".into(),
                );
                break;
            }
            budget.snapshots_remaining -= 1;
            let snapshot_name = if snapshot.name.is_empty() {
                format!("XID {}", snapshot.xid)
            } else {
                snapshot.name.clone()
            };
            let snapshot_volume = match mount_snapshot(reader, &volume, snapshot, block_size) {
                Ok(volume) => volume,
                Err(error) => {
                    warning(
                        &mut warnings,
                        format!("卷宗 {volume_name} 的快照 {snapshot_name} 无法挂载：{error}"),
                    );
                    continue;
                }
            };
            let walked = walk_volume(
                reader,
                &snapshot_volume,
                Some(snapshot_name.clone()),
                partition.start,
                block_size,
                container_size,
                &candidate_ranges,
                &exact_candidates,
                &mut budget,
            );
            merge_walk(
                &mut snapshots,
                walked,
                &mut warnings,
                &format!("卷宗 {volume_name} 的快照 {snapshot_name}"),
            );
        }
    }

    current.normalize();
    snapshots.normalize();
    if let Some(free) = &mut free {
        free.normalize();
    }
    Ok(ContainerEvidence {
        current,
        snapshots,
        free,
        container_size,
        warnings,
    })
}

fn candidate_ranges(candidates: &[&DeviceCandidate]) -> RangeSet {
    let mut ranges = RangeSet::default();
    for candidate in candidates {
        if let Some(end) = candidate.source_offset.checked_add(candidate.size) {
            ranges.add(candidate.source_offset, end);
        }
    }
    ranges.normalize();
    ranges
}

fn exact_candidates(candidates: &[&DeviceCandidate]) -> ExactCandidates {
    let mut exact = ExactCandidates::new();
    for candidate in candidates {
        if candidate.size > 0 {
            exact
                .entry((candidate.source_offset, candidate.size))
                .or_default()
                .push(candidate.id.clone());
        }
    }
    exact
}

fn free_ranges(
    runs: Vec<(u64, u64)>,
    partition_start: u64,
    block_size: usize,
    container_size: u64,
    candidates: &RangeSet,
) -> Result<RangeSet> {
    let mut ranges = RangeSet::default();
    for (first, count) in runs {
        let relative_start = first
            .checked_mul(block_size as u64)
            .context("APFS 空闲块起始位置溢出")?;
        let length = count
            .checked_mul(block_size as u64)
            .context("APFS 空闲块长度溢出")?;
        let relative_end = relative_start
            .checked_add(length)
            .context("APFS 空闲块结束位置溢出")?;
        if relative_end > container_size {
            bail!("APFS 空闲块范围超出容器");
        }
        let start = partition_start
            .checked_add(relative_start)
            .context("APFS 空闲块磁盘偏移溢出")?;
        let end = partition_start
            .checked_add(relative_end)
            .context("APFS 空闲块磁盘结束位置溢出")?;
        if candidates.overlaps(start, end) {
            ranges.add(start, end);
        }
    }
    Ok(ranges)
}

fn read_volume<R: Read + Seek>(
    reader: &mut R,
    address: u64,
    block_size: usize,
    container_size: u64,
) -> Result<ApfsVolume> {
    let offset = address
        .checked_mul(block_size as u64)
        .context("APFS 卷宗地址溢出")?;
    let end = offset
        .checked_add(block_size as u64)
        .context("APFS 卷宗结束位置溢出")?;
    if end > container_size {
        bail!("APFS 卷宗超级块超出容器");
    }
    let mut block = vec![0_u8; block_size];
    reader.seek(SeekFrom::Start(offset))?;
    reader.read_exact(&mut block)?;
    ApfsVolume::parse(&block).context("APFS 卷宗超级块校验失败")
}

struct Budget {
    entries_remaining: usize,
    snapshots_remaining: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            entries_remaining: MAX_CONTAINER_ENTRIES,
            snapshots_remaining: MAX_CONTAINER_SNAPSHOTS,
        }
    }
}

struct WalkResult {
    evidence: ViewEvidence,
    complete: bool,
    issue: Option<String>,
}

#[allow(clippy::too_many_arguments)]
fn walk_volume<R: Read + Seek>(
    reader: &mut R,
    volume: &ApfsVolume,
    snapshot: Option<String>,
    partition_start: u64,
    block_size: usize,
    container_size: u64,
    candidate_ranges: &RangeSet,
    exact_candidates: &ExactCandidates,
    budget: &mut Budget,
) -> WalkResult {
    let mut result = WalkResult {
        evidence: ViewEvidence::default(),
        complete: true,
        issue: None,
    };
    let mut stack = vec![(ROOT_DIR_INO_NUM, String::new(), 0_usize)];
    let mut visited_directories = HashSet::from([ROOT_DIR_INO_NUM]);

    'directories: while let Some((directory_oid, parent_path, depth)) = stack.pop() {
        let entries = match list_dir(reader, volume, directory_oid, block_size) {
            Ok(entries) => entries,
            Err(error) => {
                mark_incomplete(
                    &mut result,
                    format!("目录 {directory_oid} 无法读取：{error}"),
                );
                continue;
            }
        };
        for entry in entries {
            if budget.entries_remaining == 0 {
                mark_incomplete(
                    &mut result,
                    "目录条目达到 100 万个上限，其余文件未检查".into(),
                );
                break 'directories;
            }
            budget.entries_remaining -= 1;
            if entry.name == "." || entry.name == ".." {
                continue;
            }
            let inode = match load_inode(reader, volume, entry.file_id, block_size) {
                Ok(inode) => inode,
                Err(error) => {
                    mark_incomplete(
                        &mut result,
                        format!("文件记录 {} 无法读取：{error}", entry.file_id),
                    );
                    continue;
                }
            };
            let path = format!("{parent_path}/{}", entry.name);
            if path.len() > MAX_PATH_BYTES {
                mark_incomplete(&mut result, "路径超过 4096 字节上限".into());
                continue;
            }
            match inode.mode & S_IFMT {
                S_IFDIR => {
                    if depth >= MAX_DIRECTORY_DEPTH {
                        mark_incomplete(&mut result, "目录深度超过 128 层上限".into());
                    } else if visited_directories.insert(inode.oid) {
                        stack.push((inode.oid, path, depth + 1));
                    }
                }
                S_IFREG => {
                    let Some(size) = inode.size else {
                        mark_incomplete(&mut result, format!("文件 {path} 缺少数据流大小"));
                        continue;
                    };
                    if size == 0 {
                        continue;
                    }
                    let extents = match list_extents(reader, volume, inode.private_id, block_size) {
                        Ok(extents) => extents,
                        Err(error) => {
                            mark_incomplete(
                                &mut result,
                                format!("文件 {path} 的 extent 无法读取：{error}"),
                            );
                            continue;
                        }
                    };
                    if extents.len() > MAX_EXTENTS_PER_FILE {
                        mark_incomplete(
                            &mut result,
                            format!("文件 {path} 的 extent 数量超过安全上限"),
                        );
                        continue;
                    }
                    collect_file(
                        &mut result,
                        &path,
                        volume.name(),
                        snapshot.as_deref(),
                        size,
                        &extents,
                        partition_start,
                        block_size,
                        container_size,
                        candidate_ranges,
                        exact_candidates,
                    );
                }
                _ => {}
            }
        }
    }
    result.evidence.ranges.normalize();
    result
}

#[allow(clippy::too_many_arguments)]
fn collect_file(
    result: &mut WalkResult,
    path: &str,
    volume: &str,
    snapshot: Option<&str>,
    size: u64,
    extents: &[FileExtent],
    partition_start: u64,
    block_size: usize,
    container_size: u64,
    candidate_ranges: &RangeSet,
    exact_candidates: &ExactCandidates,
) {
    let mut exact = ExactStream::new(size);
    for extent in extents {
        let range = match extent_range(extent, partition_start, block_size, container_size) {
            Ok(range) => range,
            Err(error) => {
                mark_incomplete(result, format!("文件 {path} 的 extent 无效：{error}"));
                exact.valid = false;
                continue;
            }
        };
        if candidate_ranges.overlaps(range.0, range.1) {
            result.evidence.ranges.add(range.0, range.1);
        }
        exact.push(extent, range);
    }
    let Some(start) = exact.start() else {
        return;
    };
    let Some(ids) = exact_candidates.get(&(start, size)) else {
        return;
    };
    let location = Some(Location {
        volume: volume.to_string(),
        path: path.to_string(),
        snapshot: snapshot.map(str::to_string),
    });
    for id in ids {
        match result.evidence.locations.entry(id.clone()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(location.clone());
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                if *entry.get() != location {
                    entry.insert(None);
                }
            }
        }
    }
}

fn extent_range(
    extent: &FileExtent,
    partition_start: u64,
    block_size: usize,
    container_size: u64,
) -> Result<(u64, u64)> {
    physical_extent_range(
        extent.phys_block_num,
        extent.len,
        partition_start,
        block_size,
        container_size,
    )
}

pub(super) fn physical_extent_range(
    physical_block: u64,
    length: u64,
    partition_start: u64,
    block_size: usize,
    container_size: u64,
) -> Result<(u64, u64)> {
    if length == 0 || !length.is_multiple_of(block_size as u64) {
        bail!("extent 长度不是有效的 APFS 块倍数");
    }
    if physical_block == 0 {
        bail!("稀疏空洞没有物理数据块");
    }
    let relative_start = physical_block
        .checked_mul(block_size as u64)
        .context("物理块偏移溢出")?;
    let relative_end = relative_start
        .checked_add(length)
        .context("extent 结束位置溢出")?;
    if relative_end > container_size {
        bail!("extent 超出 APFS 容器");
    }
    Ok((
        partition_start
            .checked_add(relative_start)
            .context("extent 磁盘偏移溢出")?,
        partition_start
            .checked_add(relative_end)
            .context("extent 磁盘结束位置溢出")?,
    ))
}

/// 只有逻辑连续且物理连续的主数据流才能把候选精确对应到原路径。
pub(super) struct ExactStream {
    size: u64,
    next_logical: u64,
    next_physical: Option<u64>,
    physical_start: Option<u64>,
    valid: bool,
}

impl ExactStream {
    pub(super) fn new(size: u64) -> Self {
        Self {
            size,
            next_logical: 0,
            next_physical: None,
            physical_start: None,
            valid: true,
        }
    }

    fn push(&mut self, extent: &FileExtent, physical: (u64, u64)) {
        self.push_raw(extent.logical_offset, extent.len, physical);
    }

    pub(super) fn push_raw(&mut self, logical_offset: u64, length: u64, physical: (u64, u64)) {
        if logical_offset >= self.size {
            return;
        }
        if logical_offset != self.next_logical
            || self.next_physical.is_some_and(|next| next != physical.0)
        {
            self.valid = false;
            return;
        }
        self.physical_start.get_or_insert(physical.0);
        self.next_logical = logical_offset.saturating_add(length);
        self.next_physical = Some(physical.1);
    }

    pub(super) fn start(&self) -> Option<u64> {
        (self.valid && self.next_logical >= self.size)
            .then_some(self.physical_start)
            .flatten()
    }
}

fn merge_walk(
    target: &mut ViewEvidence,
    walked: WalkResult,
    warnings: &mut Vec<String>,
    context: &str,
) {
    target.merge(walked.evidence);
    if !walked.complete {
        warning(
            warnings,
            format!(
                "{context} 仅完成部分分析：{}",
                walked.issue.unwrap_or_else(|| "部分元数据不可用".into())
            ),
        );
    }
}

fn mark_incomplete(result: &mut WalkResult, message: String) {
    result.complete = false;
    if result.issue.is_none() {
        result.issue = Some(message);
    }
}

fn warning(warnings: &mut Vec<String>, message: String) {
    const MAX_CONTAINER_WARNINGS: usize = 12;
    if warnings.len() < MAX_CONTAINER_WARNINGS {
        warnings.push(message);
    } else if warnings.len() == MAX_CONTAINER_WARNINGS {
        warnings.push("此容器的其余分析警告已省略".into());
    }
}
