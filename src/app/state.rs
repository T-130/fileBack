//! FileBack 界面状态与业务流程：磁盘选择、扫描、暂停/取消、勾选、恢复。

use crate::{
    device::{DeviceCandidate, DeviceScanReport},
    recovery::{CandidatePreview, ScanReport, SourceAnalysis},
    system::{
        LocalDisk, ScanSession, detect_system_status, ensure_report_matches_disk, list_local_disks,
        privileged_analyze, privileged_preview, privileged_recover, privileged_scan_device,
    },
};
use gpui::{Context, PathPromptOptions, Task, prelude::*};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

use super::format_size;

pub(crate) const CANDIDATES_PER_PAGE: usize = 10;

pub(crate) struct FileBackApp {
    pub(crate) disks: Vec<LocalDisk>,
    pub(crate) selected_disk: Option<String>,
    pub(crate) destination: Option<PathBuf>,
    pub(crate) scanning: bool,
    pub(crate) scan_paused: bool,
    pub(crate) recovering: bool,
    pub(crate) choosing_destination: bool,
    pub(crate) status: String,
    pub(crate) scan_report: Option<DeviceScanReport>,
    pub(crate) live_candidates: Vec<DeviceCandidate>,
    pub(crate) selected_candidates: HashSet<String>,
    pub(crate) candidate_page: usize,
    pub(crate) candidate_previews: HashMap<String, CandidatePreview>,
    pub(crate) preview_loading: bool,
    pub(crate) preview_error: Option<String>,
    pub(crate) preview_generation: u64,
    pub(crate) analyzing_sources: bool,
    pub(crate) analysis_generation: u64,
    pub(crate) recovery_report: Option<ScanReport>,
    pub(crate) system_status: String,
    pub(crate) scan_session: Option<Arc<ScanSession>>,
    pub(crate) active_task: Option<Task<()>>,
}

impl FileBackApp {
    pub(crate) fn new() -> Self {
        Self {
            disks: Vec::new(),
            selected_disk: None,
            destination: None,
            scanning: false,
            scan_paused: false,
            recovering: false,
            choosing_destination: false,
            status: "正在检测物理磁盘…".into(),
            scan_report: None,
            live_candidates: Vec::new(),
            selected_candidates: HashSet::new(),
            candidate_page: 0,
            candidate_previews: HashMap::new(),
            preview_loading: false,
            preview_error: None,
            preview_generation: 0,
            analyzing_sources: false,
            analysis_generation: 0,
            recovery_report: None,
            system_status: "正在检查快照和备份状态…".into(),
            scan_session: None,
            active_task: None,
        }
    }

    /// 窗口显示后再查询系统，避免 TCC 提示或系统命令阻塞首帧。
    pub(crate) fn initialize(&mut self, cx: &mut Context<Self>) {
        let load = cx.background_spawn(async {
            let disks = list_local_disks();
            let system_status = detect_system_status().summary();
            (disks, system_status)
        });
        cx.spawn(async move |this, cx| {
            let (disks, system_status) = load.await;
            this.update(cx, |this, cx| {
                this.system_status = system_status;
                this.apply_disks(disks, false);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn busy(&self) -> bool {
        self.scanning || self.recovering || self.choosing_destination
    }

    pub(crate) fn refresh_disks(&mut self, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        self.status = "正在刷新物理磁盘列表…".into();
        cx.notify();
        let load = cx.background_spawn(async { list_local_disks() });
        cx.spawn(async move |this, cx| {
            let disks = load.await;
            this.update(cx, |this, cx| {
                this.apply_disks(disks, true);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn apply_disks(&mut self, result: anyhow::Result<Vec<LocalDisk>>, refreshed: bool) {
        match result {
            Ok(disks) => {
                let mut invalidated = false;
                if let Some(selected) = &self.selected_disk {
                    match disks.iter().find(|disk| &disk.identifier == selected) {
                        Some(disk) => {
                            if self.scan_report.as_ref().is_some_and(|report| {
                                ensure_report_matches_disk(disk, report).is_err()
                            }) {
                                self.clear_scan_results();
                                invalidated = true;
                            }
                        }
                        None => {
                            self.selected_disk = None;
                            self.clear_scan_results();
                            invalidated = true;
                        }
                    }
                }
                self.disks = disks;
                self.status = if invalidated {
                    "磁盘连接状态已变化，旧扫描结果已清除，请重新扫描".into()
                } else if self.disks.is_empty() {
                    "未检测到可扫描的物理磁盘，请连接设备后刷新".into()
                } else if refreshed {
                    "磁盘列表已刷新".into()
                } else {
                    "第一步：选择本地磁盘，然后只读扫描".into()
                };
            }
            Err(error) => self.status = format!("磁盘检测失败：{error:#}"),
        }
    }

    pub(crate) fn select_disk(&mut self, identifier: &str, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let Some(disk) = self.disks.iter().find(|disk| disk.identifier == identifier) else {
            return;
        };
        self.status = format!("已选择 {}", disk.raw_device.display());
        self.selected_disk = Some(identifier.to_string());
        self.clear_scan_results();
        cx.notify();
    }

    fn clear_scan_results(&mut self) {
        self.scan_report = None;
        self.recovery_report = None;
        self.selected_candidates.clear();
        self.live_candidates.clear();
        self.candidate_previews.clear();
        self.candidate_page = 0;
        self.preview_loading = false;
        self.preview_error = None;
        self.preview_generation = self.preview_generation.wrapping_add(1);
        self.analyzing_sources = false;
        self.analysis_generation = self.analysis_generation.wrapping_add(1);
    }

    /// 扫描成功后独立分析来源；即使分析失败，已扫描候选和恢复入口仍然保留。
    fn start_source_analysis(&mut self, cx: &mut Context<Self>) {
        let (Some(disk), Some(report)) = (
            self.selected_disk
                .as_ref()
                .and_then(|identifier| {
                    self.disks
                        .iter()
                        .find(|disk| &disk.identifier == identifier)
                })
                .cloned(),
            self.scan_report.clone(),
        ) else {
            return;
        };
        if report.candidates.is_empty() {
            if let Some(current) = &mut self.scan_report {
                current.source_analysis.complete = true;
            }
            return;
        }

        self.analyzing_sources = true;
        self.status = format!(
            "{} 正在只读分析 APFS 当前文件、快照和空闲块…",
            scan_result_status(&report)
        );
        let generation = self.analysis_generation;
        let analyze = cx.background_spawn(async move { privileged_analyze(&disk, &report) });
        cx.spawn(async move |this, cx| {
            let result = analyze.await;
            this.update(cx, |this, cx| {
                if generation != this.analysis_generation {
                    return;
                }
                this.analyzing_sources = false;
                let Some(current) = &mut this.scan_report else {
                    return;
                };
                let base_status = scan_result_status(current);
                match result {
                    Ok(analysis) => {
                        let summary = source_analysis_summary(&analysis, current.candidates.len());
                        current.source_analysis = analysis;
                        if !this.scanning && !this.recovering && !this.choosing_destination {
                            this.status = format!("{base_status} {summary}");
                        }
                    }
                    Err(error) => {
                        let detail = format!("{error:#}");
                        log_line(&format!("APFS 来源分析不可用：{detail}"));
                        current.source_analysis = SourceAnalysis {
                            detail: Some(format!(
                                "APFS 来源分析不可用：{detail}；所有候选保持无法判断，不影响预览或恢复"
                            )),
                            ..SourceAnalysis::default()
                        };
                        if !this.scanning && !this.recovering && !this.choosing_destination {
                            this.status = format!("{base_status} 来源分析不可用，候选仍可预览和恢复");
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn load_candidate_previews(&mut self, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let Some(disk) = self
            .selected_disk
            .as_ref()
            .and_then(|identifier| {
                self.disks
                    .iter()
                    .find(|disk| &disk.identifier == identifier)
            })
            .cloned()
        else {
            return;
        };
        let Some(report) = self.scan_report.clone() else {
            return;
        };
        let start = self.candidate_page * CANDIDATES_PER_PAGE;
        let ids: HashSet<String> = report
            .candidates
            .iter()
            .skip(start)
            .take(CANDIDATES_PER_PAGE)
            .filter(|candidate| !self.candidate_previews.contains_key(&candidate.id))
            .map(|candidate| candidate.id.clone())
            .collect();
        if ids.is_empty() {
            self.preview_loading = false;
            self.preview_error = None;
            return;
        }
        let mut preview_report = report;
        preview_report
            .candidates
            .retain(|candidate| ids.contains(&candidate.id));
        let generation = self.preview_generation;
        self.preview_loading = true;
        self.preview_error = None;
        cx.notify();
        let load = cx.background_spawn(async move { privileged_preview(&disk, &preview_report) });
        cx.spawn(async move |this, cx| {
            let result = load.await;
            this.update(cx, |this, cx| {
                if generation != this.preview_generation {
                    return;
                }
                this.preview_loading = false;
                match result {
                    Ok(previews) => {
                        for preview in previews {
                            this.candidate_previews.insert(preview.id.clone(), preview);
                        }
                    }
                    Err(error) => {
                        let message = format!("候选预览不可用：{error:#}");
                        log_line(&message);
                        this.preview_error = Some(message);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn choose_destination(&mut self, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let Some(disk) = self
            .selected_disk
            .as_ref()
            .and_then(|identifier| {
                self.disks
                    .iter()
                    .find(|disk| &disk.identifier == identifier)
            })
            .cloned()
        else {
            self.status = "请先选择源磁盘，再选择另一块磁盘上的恢复位置".into();
            cx.notify();
            return;
        };
        self.choosing_destination = true;
        self.status = "请选择另一块磁盘上的恢复位置；确认后会自动新建独立目录".into();
        cx.notify();
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择恢复位置（自动新建独立目录）".into()),
        });
        cx.spawn(async move |this, cx| {
            let path = match receiver.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) => {
                    this.update(cx, |this, cx| {
                        this.choosing_destination = false;
                        this.status = "已取消选择，恢复位置保持不变".into();
                        cx.notify();
                    })
                    .ok();
                    return;
                }
                Ok(Err(error)) => {
                    Self::show_error(&this, cx, error.to_string());
                    return;
                }
                Err(error) => {
                    Self::show_error(&this, cx, error.to_string());
                    return;
                }
            };
            let Some(location) = path else {
                this.update(cx, |this, cx| {
                    this.choosing_destination = false;
                    this.status = "未取得恢复位置，请重新选择".into();
                    cx.notify();
                })
                .ok();
                return;
            };
            let validation = cx
                .background_spawn(async move {
                    crate::system::ensure_destination_not_on_disk(&disk.identifier, &location)?;
                    crate::system::plan_recovery_destination(&location)
                })
                .await;
            this.update(cx, |this, cx| {
                this.choosing_destination = false;
                match validation {
                    Ok(destination) => {
                        this.destination = Some(destination);
                        this.recovery_report = None;
                        this.status = "恢复位置已验证；确认后会新建独立目录，只写入勾选文件".into();
                    }
                    Err(error) => {
                        let message = format!("无法使用恢复位置：{error:#}");
                        log_line(&message);
                        this.status = message;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn show_error(this: &gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp, message: String) {
        log_line(&format!("操作失败：{message}"));
        this.update(cx, |this, cx| {
            this.choosing_destination = false;
            this.status = format!("操作失败：{message}");
            cx.notify();
        })
        .ok();
    }

    /// 暂停/继续扫描：只向本次私有扫描会话下发指令。
    pub(crate) fn toggle_pause(&mut self, cx: &mut Context<Self>) {
        if !self.scanning {
            return;
        }
        let Some(session) = self.scan_session.as_ref() else {
            self.status = "扫描会话不可用，请重新开始扫描".into();
            cx.notify();
            return;
        };
        let result = if self.scan_paused {
            session.resume()
        } else {
            session.pause()
        };
        match result {
            Ok(()) if self.scan_paused => {
                self.scan_paused = false;
                self.status = "继续扫描…".into();
            }
            Ok(()) => {
                self.scan_paused = true;
                self.status = "已暂停（扫描位置已保留，可随时继续）".into();
            }
            Err(error) => self.status = format!("无法更新扫描状态：{error:#}"),
        }
        cx.notify();
    }

    /// 取消扫描：助手在下一个读取或雕刻块处停止。
    pub(crate) fn cancel_scan(&mut self, cx: &mut Context<Self>) {
        if !self.scanning {
            return;
        }
        let Some(session) = self.scan_session.as_ref() else {
            self.status = "扫描会话不可用，请重新开始扫描".into();
            cx.notify();
            return;
        };
        match session.cancel() {
            Ok(()) => {
                self.scan_paused = false;
                self.status = "正在取消扫描…（已发现的候选文件会保留）".into();
            }
            Err(error) => self.status = format!("无法取消扫描：{error:#}"),
        }
        cx.notify();
    }

    /// 第一阶段：只读扫描，不写任何恢复文件。
    pub(crate) fn start_scan(&mut self, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let disk = self
            .selected_disk
            .as_ref()
            .and_then(|identifier| {
                self.disks
                    .iter()
                    .find(|disk| &disk.identifier == identifier)
            })
            .cloned();
        let Some(disk) = disk else {
            self.status = "请先选择物理磁盘".into();
            cx.notify();
            return;
        };
        let session = match ScanSession::new() {
            Ok(session) => Arc::new(session),
            Err(error) => {
                self.status = format!("无法准备扫描会话：{error:#}");
                cx.notify();
                return;
            }
        };

        self.scanning = true;
        self.scan_paused = false;
        self.clear_scan_results();
        self.scan_session = Some(Arc::clone(&session));
        self.status = "首次操作将经管理员确认安装受限磁盘助手；扫描保持只读，不写入恢复文件".into();
        cx.notify();

        let scan_session = Arc::clone(&session);
        let scan = cx.background_spawn(async move { privileged_scan_device(&disk, &scan_session) });
        self.active_task = Some(cx.spawn(async move |this, cx| {
            let result = scan.await;
            this.update(cx, |this, cx| {
                this.scanning = false;
                this.scan_paused = false;
                this.active_task = None;
                this.scan_session = None;
                match result {
                    Ok(report) => {
                        this.status = scan_result_status(&report);
                        // 候选文件可能包含误报，默认不选，恢复范围完全由用户确认。
                        this.selected_candidates.clear();
                        this.live_candidates.clear();
                        this.scan_report = Some(report);
                        this.load_candidate_previews(cx);
                        this.start_source_analysis(cx);
                    }
                    Err(error) => {
                        let detail = format!("{error:#}");
                        let message = format!("扫描失败：{detail}");
                        log_line(&message);
                        this.status = message;
                    }
                }
                cx.notify();
            })
            .ok();
        }));

        // 只读取新增的 JSONL 内容，避免候选很多时反复解析整个文件。
        cx.spawn(async move |this, cx| {
            let mut consumed_bytes = 0_u64;
            let mut pending = String::new();
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(2))
                    .await;
                let progress = std::fs::read_to_string(session.progress_path()).ok();
                let new_candidates = read_candidate_updates(
                    session.candidates_path(),
                    &mut consumed_bytes,
                    &mut pending,
                );
                let keep_polling = this
                    .update(cx, |this, cx| {
                        if !this.scanning {
                            return false;
                        }
                        this.live_candidates.extend(new_candidates);
                        if this.scan_paused {
                            return true;
                        }
                        if let Some(text) = progress
                            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
                        {
                            let total = value["total"].as_u64().unwrap_or(1).max(1);
                            let scanned = value["scanned"].as_u64().unwrap_or(0).min(total);
                            let percent = scanned as f64 * 100.0 / total as f64;
                            this.status = format!(
                                "扫描中：{percent:.1}%（{} / {}）· 已发现 {} 个候选文件",
                                format_size(scanned),
                                format_size(total),
                                this.live_candidates.len()
                            );
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep_polling {
                    break;
                }
            }
        })
        .detach();
    }

    pub(crate) fn toggle_candidate(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.busy()
            || !self
                .scan_report
                .as_ref()
                .is_some_and(|report| report.candidates.iter().any(|candidate| candidate.id == id))
        {
            return;
        }
        if !self.selected_candidates.remove(id) {
            self.selected_candidates.insert(id.to_string());
        }
        cx.notify();
    }

    pub(crate) fn previous_candidate_page(&mut self, cx: &mut Context<Self>) {
        if !self.busy() && self.candidate_page > 0 {
            self.candidate_page -= 1;
            self.preview_generation = self.preview_generation.wrapping_add(1);
            cx.notify();
            self.load_candidate_previews(cx);
        }
    }

    pub(crate) fn next_candidate_page(&mut self, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let page_count = self
            .scan_report
            .as_ref()
            .map(|report| report.candidates.len().div_ceil(CANDIDATES_PER_PAGE))
            .unwrap_or(0);
        if self.candidate_page + 1 < page_count {
            self.candidate_page += 1;
            self.preview_generation = self.preview_generation.wrapping_add(1);
            cx.notify();
            self.load_candidate_previews(cx);
        }
    }

    pub(crate) fn toggle_page_selection(&mut self, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let start = self.candidate_page * CANDIDATES_PER_PAGE;
        let ids: Vec<String> = self
            .scan_report
            .as_ref()
            .map(|report| {
                report
                    .candidates
                    .iter()
                    .skip(start)
                    .take(CANDIDATES_PER_PAGE)
                    .map(|candidate| candidate.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let should_select = ids.iter().any(|id| !self.selected_candidates.contains(id));
        for id in ids {
            if should_select {
                self.selected_candidates.insert(id);
            } else {
                self.selected_candidates.remove(&id);
            }
        }
        cx.notify();
    }

    /// 第二阶段：二次明确确认，只恢复勾选的候选文件。
    pub(crate) fn start_recovery(&mut self, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let disk = self
            .selected_disk
            .as_ref()
            .and_then(|identifier| {
                self.disks
                    .iter()
                    .find(|disk| &disk.identifier == identifier)
            })
            .cloned();
        let (Some(disk), Some(report), Some(destination)) =
            (disk, self.scan_report.clone(), self.destination.clone())
        else {
            self.status = "请先完成扫描、勾选候选文件并选择恢复位置".into();
            cx.notify();
            return;
        };
        if let Err(error) = ensure_report_matches_disk(&disk, &report) {
            self.status = format!("无法恢复：{error:#}");
            cx.notify();
            return;
        }
        if self.selected_candidates.is_empty() {
            self.status = "请至少勾选一个候选文件".into();
            cx.notify();
            return;
        }

        let mut selected = report.clone();
        selected
            .candidates
            .retain(|candidate| self.selected_candidates.contains(&candidate.id));
        if selected.candidates.len() != self.selected_candidates.len() {
            self.status = "候选选择状态已失效，请重新扫描".into();
            cx.notify();
            return;
        }

        self.recovering = true;
        self.recovery_report = None;
        self.status = "将按你的确认只恢复勾选的文件，不覆盖已有文件".into();
        cx.notify();

        let recover =
            cx.background_spawn(async move { privileged_recover(&disk, &selected, &destination) });
        self.active_task = Some(cx.spawn(async move |this, cx| {
            let result = recover.await;
            this.update(cx, |this, cx| {
                this.recovering = false;
                this.active_task = None;
                match result {
                    Ok(report) => {
                        this.status = format!("恢复完成：已写入 {} 个文件", report.files.len());
                        this.recovery_report = Some(report);
                    }
                    Err(error) => {
                        let message = format!("恢复失败：{error:#}");
                        log_line(&message);
                        this.status = message;
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub(crate) fn reveal_output(&mut self, cx: &mut Context<Self>) {
        if let Some(report) = &self.recovery_report {
            cx.reveal_path(&report.output_dir);
        }
    }
}

fn read_candidate_updates(
    path: &std::path::Path,
    position: &mut u64,
    pending: &mut String,
) -> Vec<DeviceCandidate> {
    use std::io::{Read, Seek, SeekFrom};

    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    if file.metadata().map(|metadata| metadata.len()).unwrap_or(0) < *position {
        *position = 0;
        pending.clear();
    }
    if file.seek(SeekFrom::Start(*position)).is_err() {
        return Vec::new();
    }
    let mut update = String::new();
    if file.read_to_string(&mut update).is_err() {
        return Vec::new();
    }
    *position += update.len() as u64;
    pending.push_str(&update);
    let Some(last_newline) = pending.rfind('\n') else {
        return Vec::new();
    };
    let remaining = pending.split_off(last_newline + 1);
    let mut candidates = Vec::new();
    for line in pending.lines().filter(|line| !line.is_empty()) {
        match serde_json::from_str(line) {
            Ok(candidate) => candidates.push(candidate),
            Err(error) => log_line(&format!("忽略无效的候选记录：{error}")),
        }
    }
    *pending = remaining;
    candidates
}

fn scan_result_status(report: &DeviceScanReport) -> String {
    if report.canceled {
        format!(
            "扫描已取消：保留 {} 个候选文件；结果不完整，请谨慎选择或重新扫描。",
            report.candidates.len()
        )
    } else {
        format!(
            "扫描完成：找到 {} 个完整候选文件；{} 个不完整；{} 个超过大小限制。",
            report.candidates.len(),
            report.incomplete_candidates,
            report.oversized_candidates
        )
    }
}

fn source_analysis_summary(analysis: &SourceAnalysis, total: usize) -> String {
    use crate::recovery::CandidateSourceState;
    let count = |state| {
        analysis
            .candidates
            .values()
            .filter(|source| source.state == state)
            .count()
    };
    let present = count(CandidateSourceState::Present);
    let snapshots = count(CandidateSourceState::Snapshot);
    let deleted = count(CandidateSourceState::ProbablyDeleted);
    let unknown = total.saturating_sub(present + snapshots + deleted);
    format!(
        "来源分析：现存 {present} · 快照 {snapshots} · 疑似已删除 {deleted} · 无法判断 {unknown}。请结合证据勾选文件。"
    )
}

fn log_line(message: &str) {
    eprintln!("FileBack: {message}");
}
