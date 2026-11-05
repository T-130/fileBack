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

use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf};

const DEFAULT_MAX_FILE_SIZE: usize = 4 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum FileKind {
    Jpeg,
    Png,
    Pdf,
    Docx,
    Xlsx,
    Pptx,
    Zip,
    Mp4,
    Mov,
    M4a,
    Avi,
    Wav,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileCategory {
    Images,
    Documents,
    Spreadsheets,
    Presentations,
    Videos,
    Audio,
    Archives,
}

impl FileCategory {
    pub const ALL: [Self; 7] = [
        Self::Images,
        Self::Documents,
        Self::Spreadsheets,
        Self::Presentations,
        Self::Videos,
        Self::Audio,
        Self::Archives,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Images => "图片",
            Self::Documents => "文档",
            Self::Spreadsheets => "表格",
            Self::Presentations => "演示",
            Self::Videos => "视频",
            Self::Audio => "音频",
            Self::Archives => "压缩包",
        }
    }

    pub(crate) fn directory(self) -> &'static str {
        match self {
            Self::Images => "images",
            Self::Documents => "documents",
            Self::Spreadsheets => "spreadsheets",
            Self::Presentations => "presentations",
            Self::Videos => "videos",
            Self::Audio => "audio",
            Self::Archives => "archives",
        }
    }
}

impl FileKind {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Pdf => "pdf",
            Self::Docx => "docx",
            Self::Xlsx => "xlsx",
            Self::Pptx => "pptx",
            Self::Zip => "zip",
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
            Self::M4a => "m4a",
            Self::Avi => "avi",
            Self::Wav => "wav",
        }
    }

    pub fn category(self) -> FileCategory {
        match self {
            Self::Jpeg | Self::Png => FileCategory::Images,
            Self::Pdf | Self::Docx => FileCategory::Documents,
            Self::Xlsx => FileCategory::Spreadsheets,
            Self::Pptx => FileCategory::Presentations,
            Self::Mp4 | Self::Mov | Self::Avi => FileCategory::Videos,
            Self::M4a | Self::Wav => FileCategory::Audio,
            Self::Zip => FileCategory::Archives,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Jpeg => "JPEG",
            Self::Png => "PNG",
            Self::Pdf => "PDF",
            Self::Docx => "Word（DOCX）",
            Self::Xlsx => "Excel（XLSX）",
            Self::Pptx => "PowerPoint（PPTX）",
            Self::Zip => "ZIP",
            Self::Mp4 => "MP4",
            Self::Mov => "QuickTime（MOV）",
            Self::M4a => "M4A",
            Self::Avi => "AVI",
            Self::Wav => "WAV",
        }
    }

    pub(crate) fn header(self) -> &'static [u8] {
        match self {
            Self::Jpeg => b"\xff\xd8\xff",
            Self::Png => b"\x89PNG\r\n\x1a\n",
            Self::Pdf => b"%PDF-",
            Self::Zip | Self::Docx | Self::Xlsx | Self::Pptx => b"PK\x03\x04",
            Self::Mp4 | Self::Mov | Self::M4a => b"ftyp",
            Self::Avi => b"AVI ",
            Self::Wav => b"WAVE",
        }
    }

    pub(crate) fn footer(self) -> Option<&'static [u8]> {
        match self {
            Self::Jpeg => Some(b"\xff\xd9"),
            Self::Png => Some(b"IEND\xaeB`\x82"),
            Self::Pdf => Some(b"%%EOF"),
            Self::Zip | Self::Docx | Self::Xlsx | Self::Pptx => Some(b"PK\x05\x06"),
            Self::Mp4 | Self::Mov | Self::M4a | Self::Avi | Self::Wav => None,
        }
    }

    pub(crate) fn is_zip_container(self) -> bool {
        matches!(self, Self::Zip | Self::Docx | Self::Xlsx | Self::Pptx)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RecoveredFile {
    pub path: PathBuf,
    pub kind: FileKind,
    pub size: u64,
    pub source_offset: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScanReport {
    pub source: PathBuf,
    pub output_dir: PathBuf,
    pub scanned_bytes: u64,
    pub files: Vec<RecoveredFile>,
    pub incomplete_candidates: usize,
    pub oversized_candidates: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DeviceCandidate {
    pub id: String,
    pub kind: FileKind,
    pub size: u64,
    pub source_offset: u64,
}

/// 按需生成的候选预览；只包含小型缩略图和辅助元数据，不代表原文件名已恢复。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CandidatePreview {
    pub id: String,
    pub title: Option<String>,
    pub details: Vec<String>,
    pub thumbnail_png: Option<Vec<u8>>,
    pub error: Option<String>,
}

/// 候选字节的 APFS 来源证据，不把空闲块直接等同于确定删除。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateSourceState {
    Present,
    Snapshot,
    ProbablyDeleted,
    #[default]
    Unknown,
}

impl CandidateSourceState {
    pub const ALL: [Self; 4] = [
        Self::Present,
        Self::Snapshot,
        Self::ProbablyDeleted,
        Self::Unknown,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Present => "现存文件引用",
            Self::Snapshot => "快照中可恢复",
            Self::ProbablyDeleted => "疑似已删除",
            Self::Unknown => "无法判断",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CandidateSource {
    #[serde(default)]
    pub state: CandidateSourceState,
    /// 判断依据；无法判断时解释缺少什么证据。
    #[serde(default)]
    pub evidence: String,
    /// 仅在与一个未碎片化文件精确对应时提供，不把签名匹配猜成原路径。
    #[serde(default)]
    pub volume: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub snapshot: Option<String>,
}

/// 一次扫描对应的来源分析。旧报告没有此字段，所有候选默认保持“无法判断”。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SourceAnalysis {
    #[serde(default)]
    pub candidates: HashMap<String, CandidateSource>,
    /// 所有 APFS 容器、卷和计划内快照均已完成分析。
    #[serde(default)]
    pub complete: bool,
    /// 不支持、部分解析或资源上限说明；不会阻断候选恢复。
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DeviceScanReport {
    pub source: PathBuf,
    pub source_size: u64,
    /// 扫描时的物理盘身份；旧报告没有该字段，必须重新扫描后才能恢复。
    #[serde(default)]
    pub source_fingerprint: String,
    pub candidates: Vec<DeviceCandidate>,
    pub incomplete_candidates: usize,
    pub oversized_candidates: usize,
    /// 扫描被用户取消时为 true：candidates 里是取消前已发现的部分结果。
    #[serde(default)]
    pub canceled: bool,
    #[serde(default)]
    pub source_analysis: SourceAnalysis,
}

#[derive(Clone, Copy, Debug)]
pub struct ScanOptions {
    pub require_distinct_device: bool,
    pub max_file_size: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            require_distinct_device: true,
            max_file_size: DEFAULT_MAX_FILE_SIZE,
        }
    }
}
