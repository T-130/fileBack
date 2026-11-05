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

use super::{disk::LocalDisk, helper};
use crate::recovery::{DeviceScanReport, ScanReport};
use anyhow::{Context, Result, bail};
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};
use tempfile::TempDir;

/// 单次扫描的私有协作目录，避免多个进程共享固定临时文件。
pub struct ScanSession {
    _directory: TempDir,
    progress: PathBuf,
    candidates: PathBuf,
    control: PathBuf,
}

impl ScanSession {
    pub fn new() -> Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("fileback-scan-")
            .tempdir()
            .context("无法创建私有扫描会话目录")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                .context("无法限制扫描会话目录权限")?;
        }
        let session = Self {
            progress: directory.path().join("progress.json"),
            candidates: directory.path().join("candidates.jsonl"),
            control: directory.path().join("control"),
            _directory: directory,
        };
        session.resume()?;
        Ok(session)
    }

    pub fn progress_path(&self) -> &Path {
        &self.progress
    }

    pub fn candidates_path(&self) -> &Path {
        &self.candidates
    }

    pub fn pause(&self) -> Result<()> {
        self.write_control(b"pause")
    }

    pub fn resume(&self) -> Result<()> {
        self.write_control(b"")
    }

    pub fn cancel(&self) -> Result<()> {
        self.write_control(b"cancel")
    }

    fn write_control(&self, value: &[u8]) -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options
            .open(&self.control)
            .context("无法写入扫描控制指令")?;
        file.write_all(value).context("无法写入扫描控制指令")
    }
}

/// 第一阶段：只读扫描磁盘，记录候选文件偏移，不写入任何恢复文件。
pub fn privileged_scan_device(disk: &LocalDisk, session: &ScanSession) -> Result<DeviceScanReport> {
    ensure_helper()?;
    let mut report = helper::scan(
        disk,
        [
            session.progress_path(),
            session.candidates_path(),
            &session.control,
        ],
    )?;
    report.source_fingerprint = disk.fingerprint.clone();
    Ok(report)
}

/// 按需读取当前页候选，仅返回受限缩略图和元数据，不写入恢复文件。
pub fn privileged_preview(
    disk: &LocalDisk,
    report: &DeviceScanReport,
) -> Result<Vec<crate::recovery::CandidatePreview>> {
    super::ensure_report_matches_disk(disk, report)?;
    ensure_helper()?;
    helper::preview(disk, report)
}

/// 只读补充候选的 APFS 来源证据；失败由界面降级为“无法判断”。
pub fn privileged_analyze(
    disk: &LocalDisk,
    report: &DeviceScanReport,
) -> Result<crate::recovery::SourceAnalysis> {
    super::ensure_report_matches_disk(disk, report)?;
    ensure_helper()?;
    helper::analyze(disk, report)
}

/// 第二阶段：二次确认后，只把扫描报告中的候选文件写入空目录。
pub fn privileged_recover(
    disk: &LocalDisk,
    report: &DeviceScanReport,
    output_dir: &Path,
) -> Result<ScanReport> {
    super::ensure_report_matches_disk(disk, report)?;
    super::ensure_destination_not_on_disk(&disk.identifier, output_dir)?;
    ensure_helper()?;
    helper::recover(disk, report, output_dir)
}

#[cfg(target_os = "macos")]
fn cli_helper() -> Result<PathBuf> {
    let executable = std::env::current_exe().context("无法定位当前应用")?;
    let directory = executable.parent().context("无法定位应用目录")?;
    let cli = directory.join("fileback-cli");
    if !cli.is_file() {
        bail!("找不到磁盘访问助手：{}", cli.display());
    }
    Ok(cli)
}

#[cfg(target_os = "macos")]
fn ensure_helper() -> Result<()> {
    let cli = cli_helper()?;
    let helper_hash = code_hash(&cli)?;
    let installed = Path::new("/Library/PrivilegedHelperTools/dev.fileback.FileBack.helper");
    let client_hash = code_hash(&std::env::current_exe()?)?;
    let current = installed.is_file()
        && code_hash(installed).is_ok_and(|hash| hash == helper_hash)
        && std::fs::read_to_string(format!("{}/client.hash", helper::HELPER_DIRECTORY))
            .is_ok_and(|hash| hash.trim() == client_hash)
        && Path::new(helper::HELPER_SOCKET).exists();
    if !current {
        install_helper(&cli, &client_hash)?;
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn ensure_helper() -> Result<()> {
    bail!("物理磁盘助手目前仅支持 macOS")
}

#[cfg(target_os = "macos")]
fn code_hash(path: &Path) -> Result<String> {
    let output = std::process::Command::new("/usr/bin/codesign")
        .args(["-d", "--verbose=4"])
        .arg(path)
        .output()
        .context("无法验证 FileBack 代码签名")?;
    if !output.status.success() {
        bail!("FileBack 代码签名无效：{}", path.display());
    }
    let description = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    parse_code_hash(&description)
        .map(str::to_string)
        .context("无法取得 FileBack 代码签名摘要")
}

#[cfg(target_os = "macos")]
pub(super) fn parse_code_hash(description: &str) -> Option<&str> {
    description
        .lines()
        .find_map(|line| line.trim().strip_prefix("CDHash="))
}

#[cfg(target_os = "macos")]
fn install_helper(cli: &Path, client_hash: &str) -> Result<()> {
    helper::validate_client_hash(client_hash)?;
    let stage = tempfile::Builder::new()
        .prefix("fileback-helper-install-")
        .tempdir()
        .context("无法准备磁盘助手安装文件")?;
    let staged_cli = stage.path().join("helper");
    let staged_plist = stage.path().join("helper.plist");
    std::fs::copy(cli, &staged_cli).context("无法暂存磁盘访问助手")?;
    let plist = include_str!("../../packaging/macos/dev.fileback.FileBack.helper.plist")
        .replace("__CLIENT_HASH__", client_hash);
    std::fs::write(&staged_plist, plist).context("无法暂存磁盘助手配置")?;
    std::fs::write(stage.path().join("client.hash"), format!("{client_hash}\n"))?;

    let command = install_command(
        &staged_cli,
        &staged_plist,
        &stage.path().join("client.hash"),
    );
    let script = format!(
        "do shell script \"{}\" with administrator privileges",
        escape_applescript(&command)
    );
    let output = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .output()
        .context("无法请求磁盘助手安装授权")?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        bail!("磁盘助手安装未完成：{}", message.trim());
    }
    // bootstrap 是异步的；launchd 注册成功后，套接字可能稍后才可见。
    for _ in 0..50 {
        if Path::new(helper::HELPER_SOCKET).exists() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    bail!("磁盘助手已安装但未能在 5 秒内启动，请重新打开 FileBack 后重试")
}

#[cfg(target_os = "macos")]
pub(super) fn install_command(
    staged_cli: &Path,
    staged_plist: &Path,
    staged_client_hash: &Path,
) -> String {
    let installed = "/Library/PrivilegedHelperTools/dev.fileback.FileBack.helper";
    let plist_path = "/Library/LaunchDaemons/dev.fileback.FileBack.helper.plist";
    let label = helper::HELPER_LABEL;
    let directory = shell_quote(helper::HELPER_DIRECTORY);
    let staged_cli = shell_quote(&staged_cli.to_string_lossy());
    let installed_new = shell_quote(&format!("{installed}.new"));
    let installed = shell_quote(installed);
    let staged_plist = shell_quote(&staged_plist.to_string_lossy());
    let plist_new = shell_quote(&format!("{plist_path}.new"));
    let plist_path = shell_quote(plist_path);
    let staged_client_hash = shell_quote(&staged_client_hash.to_string_lossy());
    let client_hash = shell_quote(&format!("{}/client.hash", helper::HELPER_DIRECTORY));
    format!(
        "(/bin/launchctl bootout system/{label} >/dev/null 2>&1 || true) && \
         /usr/bin/install -d -o root -g wheel -m 0755 /Library/PrivilegedHelperTools && \
         /usr/bin/install -d -o root -g admin -m 0770 {directory} && \
         /usr/bin/install -o root -g wheel -m 0755 {staged_cli} {installed_new} && \
         /bin/mv {installed_new} {installed} && \
         /usr/bin/install -o root -g wheel -m 0644 {staged_plist} {plist_new} && \
         /bin/mv {plist_new} {plist_path} && \
         /usr/bin/install -o root -g admin -m 0640 {staged_client_hash} {client_hash} && \
         /bin/launchctl bootstrap system {plist_path}"
    )
}

pub(super) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn escape_applescript(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
