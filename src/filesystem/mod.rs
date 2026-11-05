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

//! 文件系统元数据分析：用只读证据解释候选来源，不参与候选雕刻或恢复写入。

mod apfs;
mod gpt;

use crate::recovery::{CandidateSource, CandidateSourceState, DeviceCandidate, SourceAnalysis};
use std::{
    collections::{HashMap, HashSet},
    io::{Read, Seek},
};

/// 分析整块 GPT 磁盘。任何不支持的格式或损坏都只产生“无法判断”，不会丢弃候选。
pub fn analyze_disk<R: Read + Seek>(
    mut reader: R,
    disk_size: u64,
    candidates: &[DeviceCandidate],
) -> SourceAnalysis {
    let mut analysis = SourceAnalysis::default();
    for candidate in candidates {
        analysis.candidates.insert(
            candidate.id.clone(),
            CandidateSource {
                evidence: "尚无足够的 APFS 元数据证据，不能确认文件是否被删除".into(),
                ..CandidateSource::default()
            },
        );
    }

    let mut warnings = Vec::new();
    match gpt::apfs_partitions(&mut reader, disk_size) {
        Ok(partitions) if partitions.is_empty() => {
            push_warning(
                &mut warnings,
                "未发现 GPT APFS 容器；其他文件系统暂不支持来源判断".into(),
            );
        }
        Ok(partitions) => {
            let mut assigned = HashSet::new();
            for partition in partitions {
                let scoped: Vec<&DeviceCandidate> = candidates
                    .iter()
                    .filter(|candidate| {
                        candidate_range(candidate).is_some_and(|(start, end)| {
                            start >= partition.start && end <= partition.end
                        })
                    })
                    .collect();
                for candidate in &scoped {
                    assigned.insert(candidate.id.clone());
                }
                if scoped.is_empty() {
                    continue;
                }
                match apfs::analyze_container(&mut reader, &partition, &scoped) {
                    Ok(evidence) => {
                        for warning in &evidence.warnings {
                            push_warning(&mut warnings, warning.clone());
                        }
                        for candidate in scoped {
                            if let Some((_, end)) = candidate_range(candidate)
                                && partition
                                    .start
                                    .checked_add(evidence.container_size)
                                    .is_none_or(|container_end| end > container_end)
                            {
                                push_warning(
                                    &mut warnings,
                                    format!("候选 {} 超出 APFS 容器的有效块范围", candidate.id),
                                );
                                continue;
                            }
                            analysis.candidates.insert(
                                candidate.id.clone(),
                                classify_candidate(candidate, &evidence),
                            );
                        }
                    }
                    Err(error) => push_warning(
                        &mut warnings,
                        format!(
                            "APFS 容器（字节偏移 {}）分析失败：{error:#}",
                            partition.start
                        ),
                    ),
                }
            }
            let outside = candidates
                .iter()
                .filter(|candidate| !assigned.contains(&candidate.id))
                .count();
            if outside > 0 {
                push_warning(
                    &mut warnings,
                    format!("{outside} 个候选不在可识别的 APFS 容器内，保持无法判断"),
                );
            }
        }
        Err(error) => push_warning(
            &mut warnings,
            format!("无法读取 GPT／APFS 分区信息：{error:#}"),
        ),
    }

    analysis.complete = warnings.is_empty();
    if !warnings.is_empty() {
        analysis.detail = Some(format!(
            "{}。未获得充分证据的候选均显示“无法判断”，不会影响预览或恢复",
            warnings.join("；")
        ));
    }
    analysis
}

fn push_warning(warnings: &mut Vec<String>, message: String) {
    const MAX_WARNINGS: usize = 12;
    if warnings.len() < MAX_WARNINGS {
        warnings.push(message);
    } else if warnings.len() == MAX_WARNINGS {
        warnings.push("其余来源分析警告已省略".into());
    }
}

fn candidate_range(candidate: &DeviceCandidate) -> Option<(u64, u64)> {
    let end = candidate.source_offset.checked_add(candidate.size)?;
    (candidate.size > 0).then_some((candidate.source_offset, end))
}

fn classify_candidate(
    candidate: &DeviceCandidate,
    evidence: &ContainerEvidence,
) -> CandidateSource {
    let Some((start, end)) = candidate_range(candidate) else {
        return CandidateSource {
            evidence: "候选范围无效，无法分析来源".into(),
            ..CandidateSource::default()
        };
    };

    if evidence.current.ranges.covers(start, end) {
        let location = evidence
            .current
            .locations
            .get(&candidate.id)
            .cloned()
            .flatten();
        return sourced_candidate(
            CandidateSourceState::Present,
            location,
            "候选范围仍被现存文件的 APFS 数据块完整覆盖",
            "候选范围与现存文件的连续数据流精确对应",
        );
    }
    if evidence.snapshots.ranges.covers(start, end) {
        let location = evidence
            .snapshots
            .locations
            .get(&candidate.id)
            .cloned()
            .flatten();
        return sourced_candidate(
            CandidateSourceState::Snapshot,
            location,
            "当前文件未完整引用该范围，但 APFS 快照保留了对应数据块",
            "候选范围与快照中文件的连续数据流精确对应",
        );
    }
    if evidence
        .free
        .as_ref()
        .is_some_and(|free| free.covers(start, end))
    {
        return CandidateSource {
            state: CandidateSourceState::ProbablyDeleted,
            evidence: "候选范围当前全部位于 APFS 空闲块；这是删除后的常见迹象，但不能排除其他未分配数据，也不保证内容仍可完整恢复".into(),
            ..CandidateSource::default()
        };
    }

    let partial = evidence.current.ranges.overlaps(start, end)
        || evidence.snapshots.ranges.overlaps(start, end)
        || evidence
            .free
            .as_ref()
            .is_some_and(|free| free.overlaps(start, end));
    CandidateSource {
        evidence: if partial {
            "候选范围只有部分现存、快照或空闲块证据；可能碎片化、跨越多个文件或已被部分覆盖"
        } else if evidence.free.is_some() {
            "候选范围没有完整的文件引用或空闲块证据；可能仍被其他结构占用，不能据此判断是否删除"
        } else {
            "当前无法读取完整 APFS 空间管理信息，不能判断是否删除"
        }
        .into(),
        ..CandidateSource::default()
    }
}

fn sourced_candidate(
    state: CandidateSourceState,
    location: Option<Location>,
    general_evidence: &str,
    exact_evidence: &str,
) -> CandidateSource {
    let mut result = CandidateSource {
        state,
        evidence: if location.is_some() {
            exact_evidence
        } else {
            general_evidence
        }
        .into(),
        ..CandidateSource::default()
    };
    if let Some(location) = location {
        result.volume = Some(location.volume);
        result.path = Some(location.path);
        result.snapshot = location.snapshot;
    }
    result
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Location {
    volume: String,
    path: String,
    snapshot: Option<String>,
}

#[derive(Default)]
pub(crate) struct ViewEvidence {
    ranges: RangeSet,
    /// Some 是唯一精确匹配；None 表示同范围对应多个路径，不能武断命名。
    locations: HashMap<String, Option<Location>>,
}

impl ViewEvidence {
    fn merge(&mut self, other: Self) {
        self.ranges.extend(other.ranges);
        for (id, location) in other.locations {
            match self.locations.entry(id) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(location);
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    if *entry.get() != location {
                        entry.insert(None);
                    }
                }
            }
        }
    }

    fn normalize(&mut self) {
        self.ranges.normalize();
    }
}

pub(crate) struct ContainerEvidence {
    current: ViewEvidence,
    snapshots: ViewEvidence,
    free: Option<RangeSet>,
    container_size: u64,
    warnings: Vec<String>,
}

/// 已排序、合并且互不重叠的半开字节范围 `[start, end)`。
#[derive(Clone, Default)]
pub(crate) struct RangeSet {
    ranges: Vec<(u64, u64)>,
}

impl RangeSet {
    fn add(&mut self, start: u64, end: u64) {
        if start < end {
            self.ranges.push((start, end));
        }
    }

    fn extend(&mut self, other: Self) {
        self.ranges.extend(other.ranges);
    }

    fn normalize(&mut self) {
        self.ranges.sort_unstable_by_key(|range| range.0);
        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(self.ranges.len());
        for (start, end) in self.ranges.drain(..) {
            if let Some(last) = merged.last_mut()
                && start <= last.1
            {
                last.1 = last.1.max(end);
                continue;
            }
            merged.push((start, end));
        }
        self.ranges = merged;
    }

    fn covers(&self, start: u64, end: u64) -> bool {
        if start >= end {
            return false;
        }
        let index = self.ranges.partition_point(|range| range.0 <= start);
        index > 0 && self.ranges[index - 1].1 >= end
    }

    fn overlaps(&self, start: u64, end: u64) -> bool {
        if start >= end {
            return false;
        }
        let index = self.ranges.partition_point(|range| range.0 <= start);
        (index > 0 && self.ranges[index - 1].1 > start)
            || self.ranges.get(index).is_some_and(|range| range.0 < end)
    }
}

#[cfg(test)]
mod tests;
