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

use crate::recovery::DeviceScanReport;
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

const DISKUTIL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalDisk {
    pub identifier: String,
    pub name: String,
    pub size: u64,
    pub internal: bool,
    pub raw_device: PathBuf,
    pub fingerprint: String,
}

pub fn list_local_disks() -> Result<Vec<LocalDisk>> {
    let list = diskutil_json(&["list", "-plist"])?;
    let identifiers = list
        .get("WholeDisks")
        .and_then(Value::as_array)
        .context("diskutil 未返回 WholeDisks")?;

    let mut disks = Vec::new();
    let mut last_error = None;
    for identifier in identifiers.iter().filter_map(Value::as_str) {
        // 单个异常或已断开的设备不能阻塞其他可用磁盘的枚举。
        match diskutil_json(&["info", "-plist", identifier]) {
            Ok(info) => {
                if let Some(disk) = parse_local_disk(&info) {
                    disks.push(disk);
                }
            }
            Err(error) => last_error = Some(error),
        }
    }
    if disks.is_empty()
        && let Some(error) = last_error
    {
        return Err(error).context("无法读取物理磁盘详细信息");
    }
    Ok(disks)
}

pub(super) fn parse_local_disk(info: &Value) -> Option<LocalDisk> {
    if !info.get("WholeDisk")?.as_bool()? {
        return None;
    }
    if info.get("Content")?.as_str()? != "GUID_partition_scheme" {
        return None;
    }
    if info.get("BusProtocol").and_then(Value::as_str) == Some("Disk Image") {
        return None;
    }

    let identifier = info.get("DeviceIdentifier")?.as_str()?.to_string();
    let name = info
        .get("MediaName")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .or_else(|| info.get("IORegistryEntryName").and_then(Value::as_str))
        .unwrap_or(&identifier)
        .to_string();
    let size = info.get("Size")?.as_u64()?;
    // diskN 会在重连后复用，不能作为跨扫描与恢复阶段的设备身份。
    let fingerprint = ["DiskUUID", "MediaUUID", "IOMediaUUID"]
        .iter()
        .find_map(|key| {
            info.get(*key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(|value| format!("uuid:{value}"))
        })
        .or_else(|| {
            info.get("IORegistryEntryID")
                .and_then(Value::as_u64)
                .map(|value| format!("ioregistry:{value}"))
        })
        .unwrap_or_else(|| {
            let bus = info
                .get("BusProtocol")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let location = info
                .get("DeviceLocation")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            format!("media:{name}:{size}:{bus}:{location}")
        });
    Some(LocalDisk {
        raw_device: PathBuf::from(format!("/dev/r{identifier}")),
        identifier,
        name,
        size,
        internal: info
            .get("Internal")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        fingerprint,
    })
}

/// 扫描报告只能用于原设备；旧报告及换盘后的报告都必须重新扫描。
pub fn ensure_report_matches_disk(disk: &LocalDisk, report: &DeviceScanReport) -> Result<()> {
    if report.source != disk.raw_device || report.source_size != disk.size {
        bail!("扫描报告的源设备路径或大小已变化，请重新扫描");
    }
    if report.source_fingerprint.is_empty() {
        bail!("扫描报告缺少设备身份，请重新扫描");
    }
    if report.source_fingerprint != disk.fingerprint {
        bail!("当前磁盘与扫描时的磁盘不是同一设备，请重新扫描");
    }
    Ok(())
}

pub fn ensure_destination_not_on_disk(identifier: &str, output_dir: &Path) -> Result<()> {
    let lookup_path = volume_lookup_path(output_dir)?;
    let info = diskutil_json(&["info", "-plist", &lookup_path.to_string_lossy()])?;
    if info.get("DeviceIdentifier").and_then(Value::as_str) == Some(identifier)
        || info.get("ParentWholeDisk").and_then(Value::as_str) == Some(identifier)
        || info
            .get("APFSPhysicalStores")
            .and_then(Value::as_array)
            .is_some_and(|stores| {
                stores.iter().filter_map(Value::as_object).any(|store| {
                    store
                        .get("APFSPhysicalStore")
                        .and_then(Value::as_str)
                        .is_some_and(|device| {
                            device == identifier
                                || device
                                    .strip_prefix(identifier)
                                    .is_some_and(|suffix| suffix.starts_with('s'))
                        })
                })
            })
    {
        bail!("恢复目录位于源物理磁盘，请选择另一块磁盘");
    }
    Ok(())
}

/// diskutil 不能识别所有文件系统上的子目录，先通过 statfs 找到所属挂载卷。
#[cfg(target_os = "macos")]
pub(super) fn volume_lookup_path(path: &Path) -> Result<PathBuf> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let path = std::fs::canonicalize(nearest_existing_ancestor(path)?)
        .context("无法解析恢复位置的实际路径")?;
    let path =
        std::ffi::CString::new(path.as_os_str().as_bytes()).context("恢复位置包含无效字符")?;
    let mut info = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: path 以 NUL 结尾；info 指向足够容纳 statfs 结果的有效内存。
    if unsafe { libc::statfs(path.as_ptr(), info.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error()).context("无法读取恢复位置所属卷");
    }
    // SAFETY: statfs 成功后 info 已初始化，挂载点由系统以 NUL 结尾。
    let info = unsafe { info.assume_init() };
    let mount = unsafe { std::ffi::CStr::from_ptr(info.f_mntonname.as_ptr()) };
    let mount = PathBuf::from(std::ffi::OsString::from_vec(mount.to_bytes().to_vec()));
    if mount.as_os_str().is_empty() {
        bail!("恢复位置所属卷的挂载点为空");
    }
    Ok(mount)
}

#[cfg(not(target_os = "macos"))]
fn volume_lookup_path(path: &Path) -> Result<PathBuf> {
    nearest_existing_ancestor(path)
}

/// 只规划恢复子目录，不在用户确认写入前创建任何目录。
/// 恢复位置可以是已有文件的磁盘根目录，但恢复文件必须发布到独立的新目录。
pub fn plan_recovery_destination(location: &Path) -> Result<PathBuf> {
    if !std::fs::metadata(location)
        .with_context(|| format!("无法读取恢复位置：{}", location.display()))?
        .is_dir()
    {
        bail!("恢复位置不是目录");
    }
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("无法生成恢复目录名称")?
        .as_secs();
    for index in 0..1000_u16 {
        let suffix = if index == 0 {
            String::new()
        } else {
            format!("-{index}")
        };
        let destination = location.join(format!("FileBack-Recovery-{timestamp}{suffix}"));
        if !destination.try_exists().context("无法检查恢复目录")? {
            return Ok(destination);
        }
    }
    bail!("无法分配独立恢复目录，请选择另一个位置")
}

fn nearest_existing_ancestor(path: &Path) -> Result<PathBuf> {
    let mut current = path;
    while !current.exists() {
        current = match current.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
    }
    Ok(current.to_path_buf())
}

fn diskutil_json(arguments: &[&str]) -> Result<Value> {
    let (diskutil_status, plutil_status, stdout) = pipeline_output(
        Command::new("diskutil").args(arguments),
        Command::new("plutil").args(["-convert", "json", "-o", "-", "-"]),
        DISKUTIL_TIMEOUT,
    )?;
    if !diskutil_status.success() || !plutil_status.success() {
        bail!("diskutil 调用失败：{arguments:?}");
    }
    serde_json::from_slice(&stdout).context("无法解析磁盘信息")
}

/// 持续读取管道输出，同时有界等待两个子进程，避免系统工具无响应时永久卡住。
pub(super) fn pipeline_output(
    producer_command: &mut Command,
    converter_command: &mut Command,
    timeout: Duration,
) -> Result<(ExitStatus, ExitStatus, Vec<u8>)> {
    let mut producer = producer_command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("无法启动系统查询进程")?;
    let producer_stdout = producer.stdout.take().context("无法读取系统查询输出")?;
    let mut converter = match converter_command
        .stdin(producer_stdout)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let _ = producer.kill();
            let _ = producer.wait();
            return Err(error).context("无法启动输出转换进程");
        }
    };
    let mut converter_stdout = converter.stdout.take().context("无法读取转换进程输出")?;
    let reader = thread::spawn(move || {
        let mut output = Vec::new();
        converter_stdout.read_to_end(&mut output).map(|_| output)
    });

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let (producer_status, converter_status) = loop {
        let statuses = producer.try_wait().and_then(|producer_status| {
            converter
                .try_wait()
                .map(|converter_status| (producer_status, converter_status))
        });
        match statuses {
            Ok((Some(producer_status), Some(converter_status))) => {
                break (producer_status, converter_status);
            }
            Ok(_) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(20));
            }
            Ok(_) => {
                timed_out = true;
                let _ = producer.kill();
                let _ = converter.kill();
                let producer_status = producer.wait().context("无法结束系统查询进程")?;
                let converter_status = converter.wait().context("无法结束输出转换进程")?;
                break (producer_status, converter_status);
            }
            Err(error) => {
                let _ = producer.kill();
                let _ = converter.kill();
                let _ = producer.wait();
                let _ = converter.wait();
                let _ = reader.join();
                return Err(error).context("无法检查系统查询进程状态");
            }
        }
    };
    let stdout = reader
        .join()
        .map_err(|_| anyhow::anyhow!("读取系统查询输出时发生panic"))?
        .context("无法读取系统查询输出")?;
    if timed_out {
        bail!("系统磁盘查询超时，请检查磁盘连接后重试");
    }
    Ok((producer_status, converter_status, stdout))
}
