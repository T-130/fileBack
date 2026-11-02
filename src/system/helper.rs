//! 由 launchd 按需启动的受限物理盘助手。
//!
//! AppleScript 提权的 root 进程与主应用没有可靠的 TCC 责任关联。
//! 用户须明确授予已安装助手“完全磁盘访问权限”；AssociatedBundleIdentifiers
//! 只用于将登录项关联到 FileBack，不会传递主应用的隐私授权。

use super::disk::LocalDisk;
use crate::recovery::{CandidatePreview, DeviceScanReport, ScanReport};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub(super) const HELPER_LABEL: &str = "dev.fileback.FileBack.helper";
#[cfg(target_os = "macos")]
pub(super) const HELPER_DIRECTORY: &str = "/Library/Application Support/FileBack";
#[cfg(target_os = "macos")]
pub(super) const HELPER_SOCKET: &str = "/Library/Application Support/FileBack/helper.sock";
const MAX_REQUEST_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum HelperRequest {
    Scan {
        identifier: String,
        progress: PathBuf,
        candidates: PathBuf,
        control: PathBuf,
    },
    Preview {
        identifier: String,
        report: DeviceScanReport,
    },
    Recover {
        identifier: String,
        report: DeviceScanReport,
        destination: PathBuf,
    },
}

pub(super) fn scan(disk: &LocalDisk, paths: [&Path; 3]) -> Result<DeviceScanReport> {
    let response = request(&HelperRequest::Scan {
        identifier: disk.identifier.clone(),
        progress: paths[0].to_path_buf(),
        candidates: paths[1].to_path_buf(),
        control: paths[2].to_path_buf(),
    })?;
    serde_json::from_value(response).context("无法解析扫描结果")
}

pub(super) fn preview(
    disk: &LocalDisk,
    report: &DeviceScanReport,
) -> Result<Vec<CandidatePreview>> {
    let response = request(&HelperRequest::Preview {
        identifier: disk.identifier.clone(),
        report: report.clone(),
    })?;
    serde_json::from_value(response).context("无法解析候选预览结果")
}

pub(super) fn recover(
    disk: &LocalDisk,
    report: &DeviceScanReport,
    destination: &Path,
) -> Result<ScanReport> {
    let response = request(&HelperRequest::Recover {
        identifier: disk.identifier.clone(),
        report: report.clone(),
        destination: destination.to_path_buf(),
    })?;
    serde_json::from_value(response).context("无法解析恢复结果")
}

#[cfg(target_os = "macos")]
fn request(request: &HelperRequest) -> Result<serde_json::Value> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    let payload = serde_json::to_vec(request)?;
    if payload.len() as u64 > MAX_REQUEST_BYTES {
        bail!("助手请求超过 256 MiB 上限");
    }
    let mut stream = UnixStream::connect(HELPER_SOCKET)
        .context("磁盘访问助手未运行，请重新操作并完成管理员授权")?;
    stream.write_all(&payload).context("无法发送助手请求")?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut response = Vec::new();
    std::io::Read::by_ref(&mut stream)
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut response)
        .context("无法读取助手响应")?;
    if response.len() as u64 > MAX_REQUEST_BYTES {
        bail!("助手响应超过 256 MiB 上限");
    }
    serde_json::from_slice::<Result<serde_json::Value, String>>(&response)
        .context("磁盘访问助手响应格式无效")?
        .map_err(|message| anyhow::anyhow!(message))
}

#[cfg(not(target_os = "macos"))]
fn request(_request: &HelperRequest) -> Result<serde_json::Value> {
    bail!("物理磁盘助手目前仅支持 macOS")
}

#[cfg(target_os = "macos")]
pub fn run() -> Result<()> {
    let mut arguments = std::env::args().skip(2);
    let (Some("--client-hash"), Some(hash), None) = (
        arguments.next().as_deref(),
        arguments.next(),
        arguments.next(),
    ) else {
        bail!("磁盘助手参数无效");
    };
    if unsafe { libc::geteuid() } != 0 {
        bail!("磁盘助手必须由 launchd 以 root 身份启动");
    }
    validate_client_hash(&hash)?;
    let listener = activated_listener()?;
    let (mut stream, _) = listener.accept().context("无法接受助手连接")?;
    let (uid, gid) = peer_identity(&stream, &hash)?;
    let result = handle_request(&mut stream, uid, gid);
    let response = serde_json::to_vec(&result)?;
    std::io::Write::write_all(&mut stream, &response).context("无法发送助手响应")
}

#[cfg(target_os = "macos")]
fn activated_listener() -> Result<std::os::unix::net::UnixListener> {
    use std::os::fd::FromRawFd;
    use std::os::raw::{c_char, c_int};

    unsafe extern "C" {
        fn launch_activate_socket(
            name: *const c_char,
            fds: *mut *mut c_int,
            count: *mut usize,
        ) -> c_int;
    }
    let name = std::ffi::CString::new("fileback")?;
    let mut descriptors = std::ptr::null_mut();
    let mut count = 0;
    // SAFETY: name 以 NUL 结尾；launchd 在成功时分配描述符数组。
    let status = unsafe { launch_activate_socket(name.as_ptr(), &mut descriptors, &mut count) };
    if status != 0 {
        bail!(
            "无法激活磁盘助手套接字：{}",
            std::io::Error::from_raw_os_error(status)
        );
    }
    let result = if count == 1 && !descriptors.is_null() {
        // SAFETY: launchd 返回一个有效且由本进程持有的套接字描述符。
        Ok(unsafe { std::os::unix::net::UnixListener::from_raw_fd(*descriptors) })
    } else {
        bail!("launchd 未返回预期的单个磁盘助手套接字");
    };
    // SAFETY: launch_activate_socket 成功返回的数组由调用方释放；描述符不随数组释放。
    unsafe { libc::free(descriptors.cast()) };
    result
}

#[cfg(target_os = "macos")]
fn peer_identity(stream: &std::os::unix::net::UnixStream, hash: &str) -> Result<(u32, u32)> {
    use std::os::fd::AsRawFd;
    use std::os::raw::{c_int, c_void};

    unsafe extern "C" {
        fn getpeereid(socket: c_int, uid: *mut u32, gid: *mut u32) -> c_int;
    }
    const LOCAL_PEERPID: c_int = 0x002;
    let fd = stream.as_raw_fd();
    let mut uid = 0;
    let mut gid = 0;
    // SAFETY: fd 有效，uid/gid 指向足够容纳结果的内存。
    if unsafe { getpeereid(fd, &mut uid, &mut gid) } != 0 || uid == 0 {
        bail!("无法验证助手客户端用户");
    }
    let mut pid = 0;
    let mut length = size_of::<c_int>() as u32;
    // SAFETY: fd 有效，pid/length 指向正确的输出缓冲区。
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            LOCAL_PEERPID,
            (&mut pid as *mut c_int).cast::<c_void>(),
            &mut length,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("无法读取助手客户端进程");
    }
    verify_client_code(pid, hash)?;
    Ok((uid, gid))
}

#[cfg(target_os = "macos")]
fn verify_client_code(pid: i32, hash: &str) -> Result<()> {
    use std::ffi::CString;
    use std::os::raw::{c_char, c_void};

    type Code = *const c_void;
    type Requirement = *const c_void;
    type Dictionary = *const c_void;
    type Number = *const c_void;
    type Allocator = *const c_void;
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
        fn CFRelease(object: *const c_void);
        fn CFNumberCreate(allocator: Allocator, kind: isize, value: *const c_void) -> Number;
        fn CFStringCreateWithCString(
            allocator: Allocator,
            text: *const c_char,
            encoding: u32,
        ) -> *const c_void;
        fn CFDictionaryCreate(
            allocator: Allocator,
            keys: *const *const c_void,
            values: *const *const c_void,
            count: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> Dictionary;
    }
    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        static kSecGuestAttributePid: *const c_void;
        fn SecCodeCopyGuestWithAttributes(
            allocator: Allocator,
            attributes: Dictionary,
            flags: u32,
            guest: *mut Code,
        ) -> i32;
        fn SecRequirementCreateWithString(
            text: *const c_void,
            flags: u32,
            requirement: *mut Requirement,
        ) -> i32;
        fn SecCodeCheckValidity(code: Code, flags: u32, requirement: Requirement) -> i32;
    }
    const K_CF_NUMBER_SINT32_TYPE: isize = 3;
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    let requirement_text = CString::new(format!(
        "identifier \"dev.fileback.FileBack\" and cdhash H\"{hash}\""
    ))?;
    // SAFETY: 所有 CoreFoundation/Security 对象均检查创建结果并成对释放。
    unsafe {
        let pid_number = CFNumberCreate(
            std::ptr::null(),
            K_CF_NUMBER_SINT32_TYPE,
            (&pid as *const i32).cast(),
        );
        if pid_number.is_null() {
            bail!("无法创建客户端进程描述");
        }
        let key = kSecGuestAttributePid;
        let attributes = CFDictionaryCreate(
            std::ptr::null(),
            &key,
            &pid_number,
            1,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        );
        if attributes.is_null() {
            CFRelease(pid_number);
            bail!("无法创建客户端验证属性");
        }
        let mut code: Code = std::ptr::null();
        let copy_status =
            SecCodeCopyGuestWithAttributes(std::ptr::null(), attributes, 0, &mut code);
        CFRelease(attributes);
        CFRelease(pid_number);
        if copy_status != 0 || code.is_null() {
            bail!("无法取得客户端代码签名：{copy_status}");
        }
        let text = CFStringCreateWithCString(
            std::ptr::null(),
            requirement_text.as_ptr(),
            K_CF_STRING_ENCODING_UTF8,
        );
        if text.is_null() {
            CFRelease(code);
            bail!("无法创建客户端签名要求文本");
        }
        let mut requirement: Requirement = std::ptr::null();
        let requirement_status = SecRequirementCreateWithString(text, 0, &mut requirement);
        CFRelease(text);
        if requirement_status != 0 || requirement.is_null() {
            CFRelease(code);
            bail!("无法创建客户端签名要求：{requirement_status}");
        }
        let status = SecCodeCheckValidity(code, 0, requirement);
        CFRelease(requirement);
        CFRelease(code);
        if status != 0 {
            bail!("拒绝未经 FileBack 签名的助手请求：{status}");
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn handle_request(
    stream: &mut std::os::unix::net::UnixStream,
    uid: u32,
    gid: u32,
) -> Result<serde_json::Value, String> {
    execute_request(stream, uid, gid).map_err(|error| format!("{error:#}"))
}

#[cfg(target_os = "macos")]
fn execute_request(
    stream: &mut std::os::unix::net::UnixStream,
    uid: u32,
    gid: u32,
) -> Result<serde_json::Value> {
    use std::io::Read;
    use std::process::Command;

    let mut payload = Vec::new();
    std::io::Read::by_ref(stream)
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut payload)
        .context("无法读取助手请求")?;
    if payload.len() as u64 > MAX_REQUEST_BYTES {
        bail!("助手请求超过 256 MiB 上限");
    }
    let request: HelperRequest = serde_json::from_slice(&payload).context("助手请求格式无效")?;
    let recovering = matches!(&request, HelperRequest::Recover { .. });
    let cli = std::env::current_exe().context("无法定位已安装的磁盘助手")?;
    let output = match request {
        HelperRequest::Scan {
            identifier,
            progress,
            candidates,
            control,
        } => {
            validate_scan_session(&progress, &candidates, &control, uid)?;
            Command::new(cli)
                .arg("scan-device")
                .arg(identifier)
                .arg("--progress")
                .arg(progress)
                .arg("--candidates")
                .arg(candidates)
                .arg("--control")
                .arg(control)
                .arg("--owner")
                .arg(format!("{uid}:{gid}"))
                .arg("--json")
                .output()
                .context("无法执行只读扫描")?
        }
        HelperRequest::Preview { identifier, report } => {
            let mut report_file = tempfile::NamedTempFile::new().context("无法创建临时扫描报告")?;
            serde_json::to_writer(&mut report_file, &report).context("无法写入临时扫描报告")?;
            std::io::Write::flush(&mut report_file).context("无法同步临时扫描报告")?;
            Command::new(cli)
                .arg("preview-device")
                .arg(identifier)
                .arg(report_file.path())
                .arg("--json")
                .output()
                .context("无法执行候选预览")?
        }
        HelperRequest::Recover {
            identifier,
            report,
            destination,
        } => {
            let mut report_file = tempfile::NamedTempFile::new().context("无法创建临时扫描报告")?;
            serde_json::to_writer(&mut report_file, &report).context("无法写入临时扫描报告")?;
            std::io::Write::flush(&mut report_file).context("无法同步临时扫描报告")?;
            Command::new(cli)
                .arg("recover-device")
                .arg(identifier)
                .arg(report_file.path())
                .arg(destination)
                .arg("--json")
                .output()
                .context("无法执行候选恢复")?
        }
    };
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        let message = message.trim();
        // Rust 主函数的 Err 输出带有 "Error: "；界面已经标记失败，无需重复显示。
        bail!("{}", message.strip_prefix("Error: ").unwrap_or(message));
    }
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("无法解析助手操作结果")?;
    if recovering {
        let report: ScanReport = serde_json::from_value(value.clone())?;
        for file in report.files {
            own_recovered_file(&file.path, uid, gid)?;
        }
    }
    Ok(value)
}

#[cfg(target_os = "macos")]
fn own_recovered_file(path: &Path, uid: u32, gid: u32) -> Result<()> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .context("无法验证恢复文件")?;
    if !file.metadata()?.is_file() {
        bail!("恢复结果不是普通文件");
    }
    // SAFETY: 文件描述符已用 O_NOFOLLOW 打开，仅修改这一个恢复文件的属主。
    if unsafe { libc::fchown(file.as_raw_fd(), uid, gid) } != 0 {
        return Err(std::io::Error::last_os_error()).context("无法设置恢复文件属主");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn validate_scan_session(
    progress: &Path,
    candidates: &Path,
    control: &Path,
    uid: u32,
) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let directory = progress.parent().context("扫描会话路径无效")?;
    if candidates.parent() != Some(directory) || control.parent() != Some(directory) {
        bail!("扫描协作文件必须位于同一会话目录");
    }
    if progress.file_name().and_then(|name| name.to_str()) != Some("progress.json")
        || candidates.file_name().and_then(|name| name.to_str()) != Some("candidates.jsonl")
        || control.file_name().and_then(|name| name.to_str()) != Some("control")
    {
        bail!("扫描协作文件名无效");
    }
    let metadata = std::fs::metadata(directory).context("无法读取扫描会话目录")?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        bail!("扫描会话目录必须仅由当前用户访问");
    }
    Ok(())
}

pub(super) fn validate_client_hash(hash: &str) -> Result<()> {
    if hash.len() == 40 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        bail!("客户端签名摘要格式无效")
    }
}

#[cfg(test)]
mod tests {
    use super::{validate_client_hash, validate_scan_session};

    #[test]
    fn helper_requires_exact_signature_hash() {
        assert!(validate_client_hash("0123456789abcdef0123456789abcdef01234567").is_ok());
        assert!(validate_client_hash("not-a-code-signing-hash").is_err());
        assert!(validate_client_hash(&"a".repeat(41)).is_err());
    }

    #[test]
    fn helper_accepts_private_scan_session() {
        use std::os::unix::fs::PermissionsExt;
        let session = tempfile::TempDir::new().expect("session");
        std::fs::set_permissions(session.path(), std::fs::Permissions::from_mode(0o700))
            .expect("private permissions");

        validate_scan_session(
            &session.path().join("progress.json"),
            &session.path().join("candidates.jsonl"),
            &session.path().join("control"),
            unsafe { libc::geteuid() },
        )
        .expect("private session");
    }

    #[test]
    fn helper_rejects_paths_outside_private_scan_session() {
        let session = tempfile::TempDir::new().expect("session");
        let other = tempfile::TempDir::new().expect("other");

        let result = validate_scan_session(
            &session.path().join("progress.json"),
            &other.path().join("candidates.jsonl"),
            &session.path().join("control"),
            unsafe { libc::geteuid() },
        );

        assert!(result.is_err());
    }

    #[test]
    fn helper_rejects_unexpected_session_file_names() {
        let session = tempfile::TempDir::new().expect("session");

        let result = validate_scan_session(
            &session.path().join("progress.json"),
            &session.path().join("candidates.jsonl"),
            &session.path().join("other-file"),
            unsafe { libc::geteuid() },
        );

        assert!(result.is_err());
    }
}
