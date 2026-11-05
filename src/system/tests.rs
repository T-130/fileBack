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

use super::{disk::parse_local_disk, privilege::shell_quote, *};
use crate::recovery::DeviceScanReport;
use serde_json::json;
use std::path::{Path, PathBuf};

#[test]
fn summarizes_missing_backup() {
    let status = SystemStatus {
        local_snapshots: Some(0),
        time_machine_configured: false,
    };
    assert_eq!(
        status.summary(),
        "APFS 本地快照：0 个 · Time Machine：未检测到备份目标"
    );
}

#[test]
fn parses_physical_disk() {
    let info = json!({
        "WholeDisk": true,
        "Content": "GUID_partition_scheme",
        "BusProtocol": "Apple Fabric",
        "DeviceIdentifier": "disk0",
        "MediaName": "APPLE SSD",
        "Size": 500_000_000_000_u64,
        "Internal": true
    });
    let disk = parse_local_disk(&info).expect("physical disk");
    assert_eq!(disk.raw_device, PathBuf::from("/dev/rdisk0"));
    assert!(disk.internal);
}

#[test]
fn excludes_disk_images() {
    let info = json!({
        "WholeDisk": true,
        "Content": "GUID_partition_scheme",
        "BusProtocol": "Disk Image"
    });
    assert!(parse_local_disk(&info).is_none());
}

#[test]
fn quotes_shell_paths() {
    assert_eq!(shell_quote("/tmp/it's safe"), "'/tmp/it'\\''s safe'");
}

#[cfg(target_os = "macos")]
#[test]
fn quotes_helper_install_paths_containing_spaces() {
    let command = super::privilege::install_command(
        Path::new("/tmp/staged helper"),
        Path::new("/tmp/staged helper.plist"),
        Path::new("/tmp/client hash"),
    );

    assert!(command.contains("0770 '/Library/Application Support/FileBack'"));
    assert!(command.contains("'/tmp/staged helper'"));
    assert!(command.contains("'/Library/Application Support/FileBack/client.hash'"));
    assert!(!command.contains("0770 /Library/Application Support/FileBack"));
}

#[cfg(target_os = "macos")]
#[test]
fn parses_code_hash_from_verbose_signature_output() {
    let description = "Executable=/tmp/FileBack\n  CDHash=0123456789abcdef0123456789abcdef01234567\nSignature=adhoc";

    assert_eq!(
        super::privilege::parse_code_hash(description),
        Some("0123456789abcdef0123456789abcdef01234567")
    );
    assert_eq!(super::privilege::parse_code_hash("Signature=adhoc"), None);
}

fn test_disk() -> LocalDisk {
    LocalDisk {
        identifier: "disk2".into(),
        name: "Test Disk".into(),
        size: 4096,
        internal: false,
        raw_device: PathBuf::from("/dev/rdisk2"),
        fingerprint: "uuid:source-id".into(),
    }
}

fn test_report() -> DeviceScanReport {
    DeviceScanReport {
        source: PathBuf::from("/dev/rdisk2"),
        source_size: 4096,
        source_fingerprint: "uuid:source-id".into(),
        candidates: Vec::new(),
        incomplete_candidates: 0,
        oversized_candidates: 0,
        canceled: false,
        source_analysis: crate::recovery::SourceAnalysis::default(),
    }
}

#[test]
fn accepts_report_from_same_physical_disk() {
    ensure_report_matches_disk(&test_disk(), &test_report()).expect("matching report");
}

#[test]
fn rejects_report_from_replaced_disk_with_same_bsd_identifier() {
    let mut report = test_report();
    report.source_fingerprint = "uuid:replacement".into();

    let error = ensure_report_matches_disk(&test_disk(), &report).expect_err("must fail");

    assert!(error.to_string().contains("不是同一设备"));
}

#[test]
fn rejects_legacy_report_without_source_fingerprint() {
    let mut report = test_report();
    report.source_fingerprint.clear();

    let error = ensure_report_matches_disk(&test_disk(), &report).expect_err("must fail");

    assert!(error.to_string().contains("重新扫描"));
}

#[cfg(target_os = "macos")]
#[test]
fn resolves_nested_recovery_directory_to_its_mounted_volume() {
    let location = tempfile::TempDir::new().expect("temporary recovery location");
    let nested = location.path().join("nested");
    std::fs::create_dir(&nested).expect("nested directory");

    let mount = super::disk::volume_lookup_path(&nested).expect("mounted volume");

    assert!(mount.is_absolute());
    assert!(!mount.as_os_str().is_empty());
}

#[test]
fn plans_nested_recovery_directory_without_creating_it() {
    let location = tempfile::TempDir::new().expect("temporary recovery location");
    let nested = location.path().join("existing").join("folder");
    std::fs::create_dir_all(&nested).expect("nested location");
    std::fs::write(nested.join("existing-file"), b"keep").expect("existing file");

    let destination = plan_recovery_destination(&nested).expect("planned destination");

    assert_eq!(destination.parent(), Some(nested.as_path()));
    assert!(
        destination
            .file_name()
            .expect("directory name")
            .to_string_lossy()
            .starts_with("FileBack-Recovery-")
    );
    assert!(!destination.try_exists().expect("destination existence"));
    assert_eq!(
        std::fs::read(nested.join("existing-file")).expect("existing file"),
        b"keep"
    );
}

#[test]
fn skips_an_existing_recovery_directory() {
    let location = tempfile::TempDir::new().expect("temporary recovery location");
    let first = plan_recovery_destination(location.path()).expect("first destination");
    std::fs::create_dir(&first).expect("occupied destination");

    let second = plan_recovery_destination(location.path()).expect("alternative destination");

    assert_eq!(second.parent(), Some(location.path()));
    assert_ne!(first, second);
    assert!(!second.try_exists().expect("destination existence"));
}

#[test]
fn rejects_a_regular_file_as_recovery_location() {
    let location = tempfile::TempDir::new().expect("temporary recovery location");
    let file = location.path().join("not-a-directory");
    std::fs::write(&file, b"unchanged").expect("regular file");

    let error = plan_recovery_destination(&file).expect_err("file must be rejected");

    assert!(error.to_string().contains("不是目录"));
    assert_eq!(std::fs::read(file).expect("regular file"), b"unchanged");
}

#[test]
fn disk_fingerprint_does_not_depend_on_bsd_identifier() {
    let info = json!({
        "WholeDisk": true,
        "Content": "GUID_partition_scheme",
        "BusProtocol": "USB",
        "DeviceIdentifier": "disk7",
        "MediaName": "USB Disk",
        "DiskUUID": "stable-uuid",
        "Size": 4096_u64,
        "Internal": false
    });

    let disk = parse_local_disk(&info).expect("physical disk");

    assert_eq!(disk.fingerprint, "uuid:stable-uuid");
}

#[test]
fn scan_sessions_have_independent_controls() {
    let first = ScanSession::new().expect("first session");
    let second = ScanSession::new().expect("second session");

    first.pause().expect("pause first");

    assert_eq!(
        std::fs::read_to_string(
            first
                .progress_path()
                .parent()
                .expect("session directory")
                .join("control")
        )
        .expect("first control"),
        "pause"
    );
    assert_eq!(
        std::fs::read_to_string(
            second
                .progress_path()
                .parent()
                .expect("session directory")
                .join("control")
        )
        .expect("second control"),
        ""
    );
    assert_ne!(first.progress_path(), second.progress_path());
}

#[cfg(unix)]
#[test]
fn scan_session_directory_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let session = ScanSession::new().expect("session");
    let permissions =
        std::fs::metadata(session.progress_path().parent().expect("session directory"))
            .expect("metadata")
            .permissions();

    assert_eq!(permissions.mode() & 0o777, 0o700);
}

#[cfg(unix)]
#[test]
fn scan_control_does_not_follow_symlinks() {
    use std::os::unix::fs::symlink;

    let session = ScanSession::new().expect("session");
    let directory = session.progress_path().parent().expect("session directory");
    let control = directory.join("control");
    let victim = directory.join("victim");
    std::fs::write(&victim, b"unchanged").expect("victim");
    std::fs::remove_file(&control).expect("control");
    symlink(&victim, &control).expect("symlink");

    session.pause().expect_err("symlink must be rejected");

    assert_eq!(std::fs::read(&victim).expect("victim"), b"unchanged");
}

#[cfg(unix)]
#[test]
fn pipeline_returns_output_without_leaving_processes() {
    use std::{process::Command, time::Duration};

    let (producer, converter, stdout) = super::disk::pipeline_output(
        Command::new("/bin/sh").args(["-c", "printf pipeline-ok"]),
        &mut Command::new("/bin/cat"),
        Duration::from_secs(2),
    )
    .expect("successful pipeline");

    assert!(producer.success());
    assert!(converter.success());
    assert_eq!(stdout, b"pipeline-ok");
}

#[cfg(unix)]
#[test]
fn pipeline_kills_a_stuck_system_query_at_the_deadline() {
    use std::{
        process::Command,
        time::{Duration, Instant},
    };

    let started = Instant::now();
    let error = super::disk::pipeline_output(
        Command::new("/bin/sh").args(["-c", "exec /bin/sleep 5"]),
        &mut Command::new("/bin/cat"),
        Duration::from_millis(50),
    )
    .expect_err("stuck pipeline must time out");

    assert!(error.to_string().contains("超时"));
    assert!(started.elapsed() < Duration::from_secs(2));
}
