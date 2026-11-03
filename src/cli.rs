use anyhow::Context;
use anyhow::{Result, bail};
use fileback::{
    device::{
        DeviceCandidate, DeviceScanReport, ScanControl, ScanHooks, recover_device_candidates,
        scan_device_index,
    },
    recovery::{ScanOptions, ScanReport, scan_image},
    system::{detect_system_status, ensure_report_matches_disk, list_local_disks},
};
use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tempfile::NamedTempFile;

#[derive(Clone, Copy)]
struct FileOwner {
    uid: libc::uid_t,
    gid: libc::gid_t,
}

fn main() -> Result<()> {
    let mut arguments = env::args().skip(1);
    match arguments.next().as_deref() {
        #[cfg(target_os = "macos")]
        Some("run-helper") => fileback::system::run_privileged_helper(),
        Some("status") => {
            println!("{}", detect_system_status().summary());
            Ok(())
        }
        Some("disks") => {
            for disk in list_local_disks()? {
                println!(
                    "{} · {} · {} 字节 · {}",
                    disk.identifier,
                    disk.name,
                    disk.size,
                    if disk.internal { "内置" } else { "外接" }
                );
            }
            Ok(())
        }
        Some("scan") => {
            let Some(source) = arguments.next().map(PathBuf::from) else {
                print_usage();
                bail!("缺少源镜像路径");
            };
            let Some(destination) = arguments.next().map(PathBuf::from) else {
                print_usage();
                bail!("缺少恢复目录");
            };
            if arguments.next().is_some() {
                print_usage();
                bail!("参数过多");
            }
            let report = scan_image(&source, &destination, ScanOptions::default())?;
            print_report(&report, false)
        }
        Some("scan-device") => {
            let Some(identifier) = arguments.next() else {
                print_usage();
                bail!("缺少磁盘标识，例如 disk0");
            };
            let mut json = false;
            let mut progress_path: Option<PathBuf> = None;
            let mut candidates_path: Option<PathBuf> = None;
            let mut control_path: Option<PathBuf> = None;
            let mut owner = None;
            while let Some(argument) = arguments.next() {
                match argument.as_str() {
                    "--json" => json = true,
                    "--progress" => {
                        let Some(path) = arguments.next() else {
                            print_usage();
                            bail!("--progress 缺少文件路径");
                        };
                        progress_path = Some(PathBuf::from(path));
                    }
                    "--candidates" => {
                        let Some(path) = arguments.next() else {
                            print_usage();
                            bail!("--candidates 缺少文件路径");
                        };
                        candidates_path = Some(PathBuf::from(path));
                    }
                    "--control" => {
                        let Some(path) = arguments.next() else {
                            print_usage();
                            bail!("--control 缺少文件路径");
                        };
                        control_path = Some(PathBuf::from(path));
                    }
                    "--owner" => {
                        let Some(value) = arguments.next() else {
                            print_usage();
                            bail!("--owner 缺少用户和组编号");
                        };
                        let Some((uid, gid)) = value.split_once(':') else {
                            bail!("--owner 必须使用 uid:gid 格式");
                        };
                        owner = Some(FileOwner {
                            uid: uid.parse().context("--owner 用户编号无效")?,
                            gid: gid.parse().context("--owner 组编号无效")?,
                        });
                    }
                    _ => {
                        print_usage();
                        bail!("无法识别参数");
                    }
                }
            }
            let disk = find_disk(&identifier)?;
            let total = disk.size;
            // 多线程会同时上报进度和候选；共享锁保证每份 JSON 都完整可读。
            let output_lock = Arc::new(Mutex::new(()));
            let progress = progress_path.map(|path| -> Box<dyn Fn(u64) + Sync> {
                let output_lock = Arc::clone(&output_lock);
                Box::new(move |scanned| {
                    let _guard = output_lock
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    let content = format!("{{\"scanned\":{scanned},\"total\":{total}}}");
                    if let Err(error) = write_atomic_private(&path, content.as_bytes(), owner) {
                        eprintln!("无法写入扫描进度：{error:#}");
                    }
                })
            });
            // 未指定控制文件时始终继续；指定后读取失败必须停止，避免扫描失去控制。
            let control: Box<dyn Fn() -> ScanControl + Sync> = match control_path {
                Some(path) => Box::new(move || match read_control(&path) {
                    Ok(content) => match content.as_str() {
                        "pause" => ScanControl::Pause,
                        "cancel" => ScanControl::Cancel,
                        _ => ScanControl::Continue,
                    },
                    Err(error) => {
                        eprintln!("扫描控制文件不可用，正在停止：{error:#}");
                        ScanControl::Cancel
                    }
                }),
                None => Box::new(|| ScanControl::Continue),
            };
            // 每发现一个候选文件追加一行 JSONL，供界面实时展示。
            let candidate_reporter =
                candidates_path.map(|path| -> Box<dyn Fn(&DeviceCandidate) + Sync> {
                    let output_lock = Arc::clone(&output_lock);
                    Box::new(move |candidate| {
                        let _guard = output_lock
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        let line = match serde_json::to_string(candidate) {
                            Ok(line) => line,
                            Err(error) => {
                                eprintln!("无法序列化候选文件：{error}");
                                return;
                            }
                        };
                        match open_private_output(&path, true, owner)
                            .and_then(|mut file| writeln!(file, "{line}").context("写入候选文件"))
                        {
                            Ok(()) => {}
                            Err(error) => eprintln!("无法记录候选文件：{error:#}"),
                        }
                    })
                });
            let hooks = ScanHooks {
                progress: progress.as_deref(),
                control: Some(&*control),
                candidate: candidate_reporter.as_deref(),
            };
            let mut report =
                scan_device_index(&disk.raw_device, disk.size, ScanOptions::default(), hooks)?;
            report.source_fingerprint = disk.fingerprint.clone();
            print_scan_report(&report, json)
        }
        Some("preview-device") => {
            let Some(identifier) = arguments.next() else {
                print_usage();
                bail!("缺少磁盘标识，例如 disk0");
            };
            let Some(report_path) = arguments.next().map(PathBuf::from) else {
                print_usage();
                bail!("缺少扫描报告 JSON 路径");
            };
            let json = parse_json_flag(&mut arguments)?;
            let disk = find_disk(&identifier)?;
            let report_bytes = read_private_file(&report_path)
                .with_context(|| format!("无法读取扫描报告 {}", report_path.display()))?;
            let report: DeviceScanReport = serde_json::from_slice(&report_bytes)
                .map_err(|error| anyhow::anyhow!("扫描报告格式无效：{error}"))?;
            ensure_report_matches_disk(&disk, &report)?;
            let previews = fileback::device::preview_device_candidates(
                &disk.raw_device,
                disk.size,
                &report.candidates,
            )?;
            if json {
                println!("{}", serde_json::to_string(&previews)?);
            } else {
                for preview in previews {
                    println!(
                        "{} · {} · {}",
                        preview.id,
                        preview.title.as_deref().unwrap_or("无可用标题"),
                        preview.details.join(" · ")
                    );
                }
            }
            Ok(())
        }
        Some("analyze-device") => {
            let Some(identifier) = arguments.next() else {
                print_usage();
                bail!("缺少磁盘标识，例如 disk0");
            };
            let Some(report_path) = arguments.next().map(PathBuf::from) else {
                print_usage();
                bail!("缺少扫描报告 JSON 路径");
            };
            let json = parse_json_flag(&mut arguments)?;
            let disk = find_disk(&identifier)?;
            let report_bytes = read_private_file(&report_path)
                .with_context(|| format!("无法读取扫描报告 {}", report_path.display()))?;
            let report: DeviceScanReport = serde_json::from_slice(&report_bytes)
                .map_err(|error| anyhow::anyhow!("扫描报告格式无效：{error}"))?;
            ensure_report_matches_disk(&disk, &report)?;
            let analysis = fileback::device::analyze_device_candidates(
                &disk.raw_device,
                disk.size,
                &report.candidates,
            )?;
            if json {
                println!("{}", serde_json::to_string(&analysis)?);
            } else {
                for candidate in &report.candidates {
                    if let Some(source) = analysis.candidates.get(&candidate.id) {
                        println!(
                            "{} · {} · {}",
                            candidate.id,
                            source.state.label(),
                            source.evidence
                        );
                    }
                }
                if let Some(detail) = analysis.detail {
                    println!("来源分析说明：{detail}");
                }
            }
            Ok(())
        }
        Some("recover-device") => {
            let Some(identifier) = arguments.next() else {
                print_usage();
                bail!("缺少磁盘标识，例如 disk0");
            };
            let Some(report_path) = arguments.next().map(PathBuf::from) else {
                print_usage();
                bail!("缺少扫描报告 JSON 路径");
            };
            let Some(destination) = arguments.next().map(PathBuf::from) else {
                print_usage();
                bail!("缺少恢复目录");
            };
            let json = parse_json_flag(&mut arguments)?;
            let disk = find_disk(&identifier)?;
            let report_bytes = read_private_file(&report_path)
                .with_context(|| format!("无法读取扫描报告 {}", report_path.display()))?;
            let report: DeviceScanReport = serde_json::from_slice(&report_bytes)
                .map_err(|error| anyhow::anyhow!("扫描报告格式无效：{error}"))?;
            ensure_report_matches_disk(&disk, &report)?;
            let report = recover_device_candidates(
                &disk.raw_device,
                disk.size,
                &report.candidates,
                &destination,
                ScanOptions::default(),
            )?;
            print_report(&report, json)
        }
        _ => {
            print_usage();
            Ok(())
        }
    }
}

fn open_private_output(path: &Path, append: bool, owner: Option<FileOwner>) -> Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true);
    if append {
        options.append(true);
    } else {
        options.truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .with_context(|| format!("无法安全打开输出文件 {}", path.display()))?;
    set_file_owner(&file, owner)?;
    Ok(file)
}

fn write_atomic_private(path: &Path, content: &[u8], owner: Option<FileOwner>) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = NamedTempFile::new_in(parent).context("无法创建临时进度文件")?;
    set_file_owner(temporary.as_file(), owner)?;
    temporary
        .write_all(content)
        .and_then(|()| temporary.flush())
        .context("无法写入临时进度文件")?;
    temporary
        .persist(path)
        .with_context(|| format!("无法替换进度文件 {}", path.display()))?;
    Ok(())
}

fn read_control(path: &Path) -> Result<String> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(path).context("无法读取扫描控制文件")?;
    let mut content = String::new();
    std::io::Read::read_to_string(&mut file, &mut content).context("扫描控制文件格式无效")?;
    Ok(content)
}

const MAX_SCAN_REPORT_BYTES: u64 = 256 * 1024 * 1024;

fn read_private_file(path: &Path) -> Result<Vec<u8>> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    if file.metadata()?.len() > MAX_SCAN_REPORT_BYTES {
        bail!("扫描报告超过 256 MiB 上限");
    }
    let mut limited = std::io::Read::take(file, MAX_SCAN_REPORT_BYTES + 1);
    let mut content = Vec::new();
    std::io::Read::read_to_end(&mut limited, &mut content)?;
    if content.len() as u64 > MAX_SCAN_REPORT_BYTES {
        bail!("扫描报告超过 256 MiB 上限");
    }
    Ok(content)
}

#[cfg(unix)]
fn set_file_owner(file: &fs::File, owner: Option<FileOwner>) -> Result<()> {
    use std::os::unix::io::AsRawFd;
    if let Some(owner) = owner {
        // SAFETY: file 保持有效，fchown 只修改这个已打开的协作文件的属主。
        if unsafe { libc::fchown(file.as_raw_fd(), owner.uid, owner.gid) } != 0 {
            return Err(std::io::Error::last_os_error()).context("无法设置协作文件属主");
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_file_owner(_file: &fs::File, _owner: Option<FileOwner>) -> Result<()> {
    Ok(())
}

fn parse_json_flag(arguments: &mut impl Iterator<Item = String>) -> Result<bool> {
    let json = match arguments.next().as_deref() {
        Some("--json") => true,
        None => false,
        _ => {
            print_usage();
            bail!("无法识别参数");
        }
    };
    if arguments.next().is_some() {
        print_usage();
        bail!("参数过多");
    }
    Ok(json)
}

fn find_disk(identifier: &str) -> Result<fileback::system::LocalDisk> {
    list_local_disks()?
        .into_iter()
        .find(|disk| disk.identifier == identifier)
        .ok_or_else(|| anyhow::anyhow!("找不到物理磁盘：{identifier}"))
}

fn print_scan_report(report: &DeviceScanReport, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(report)?);
        return Ok(());
    }
    println!(
        "扫描 {} 字节：找到 {} 个完整候选文件，{} 个不完整候选，{} 个超大候选（未写入任何文件）",
        report.source_size,
        report.candidates.len(),
        report.incomplete_candidates,
        report.oversized_candidates
    );
    for candidate in &report.candidates {
        println!(
            "{} · {} · {} · {} 字节 · 源偏移 {}",
            candidate.id,
            candidate.kind.category().label(),
            candidate.kind.label(),
            candidate.size,
            candidate.source_offset
        );
    }
    Ok(())
}

fn print_report(report: &ScanReport, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(report)?);
        return Ok(());
    }
    println!(
        "扫描 {} 字节：恢复 {} 个文件，{} 个不完整候选，{} 个超大候选",
        report.scanned_bytes,
        report.files.len(),
        report.incomplete_candidates,
        report.oversized_candidates
    );
    for file in &report.files {
        println!("{} · {} 字节", file.path.display(), file.size);
    }
    Ok(())
}

fn print_usage() {
    println!("FileBack 命令行");
    println!("  fileback-cli status");
    println!("  fileback-cli disks");
    println!("  fileback-cli scan <磁盘镜像> <另一设备上的空目录>");
    println!(
        "  fileback-cli scan-device <diskN> [--json] [--progress <文件>] [--candidates <文件>] [--control <文件>]"
    );
    println!("  fileback-cli preview-device <diskN> <扫描报告JSON> [--json]");
    println!("  fileback-cli analyze-device <diskN> <扫描报告JSON> [--json]");
    println!(
        "  fileback-cli recover-device <diskN> <扫描报告JSON> <另一物理盘上的空目录> [--json]"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn atomic_progress_write_leaves_complete_json() {
        let temp = TempDir::new().expect("temp directory");
        let progress = temp.path().join("progress.json");

        write_atomic_private(&progress, br#"{"scanned":42,"total":100}"#, None).expect("progress");

        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(&progress).expect("progress")).expect("valid JSON");
        assert_eq!(value["scanned"], 42);
    }

    #[cfg(unix)]
    #[test]
    fn progress_rename_does_not_follow_symlink() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().expect("temp directory");
        let progress = temp.path().join("progress.json");
        let victim = temp.path().join("victim");
        fs::write(&victim, b"unchanged").expect("victim");
        symlink(&victim, &progress).expect("symlink");

        write_atomic_private(&progress, b"new progress", None).expect("progress");

        assert_eq!(fs::read(&victim).expect("victim"), b"unchanged");
        assert_eq!(fs::read(&progress).expect("progress"), b"new progress");
    }

    #[cfg(unix)]
    #[test]
    fn candidate_append_rejects_symlink() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().expect("temp directory");
        let candidates = temp.path().join("candidates.jsonl");
        let victim = temp.path().join("victim");
        fs::write(&victim, b"unchanged").expect("victim");
        symlink(&victim, &candidates).expect("symlink");

        open_private_output(&candidates, true, None).expect_err("symlink must fail");

        assert_eq!(fs::read(&victim).expect("victim"), b"unchanged");
    }

    #[test]
    fn rejects_oversized_scan_report_before_reading() {
        let temp = TempDir::new().expect("temp directory");
        let report = temp.path().join("scan.json");
        let file = fs::File::create(&report).expect("report");
        file.set_len(MAX_SCAN_REPORT_BYTES + 1)
            .expect("report size");

        let error = read_private_file(&report).expect_err("oversized report must fail");

        assert!(error.to_string().contains("256 MiB"));
    }
}
