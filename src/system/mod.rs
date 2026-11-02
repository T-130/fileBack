//! macOS 系统能力：磁盘发现、目标盘校验、管理员授权与备份状态。

mod disk;
mod helper;
mod privilege;
mod status;

pub use disk::{
    LocalDisk, ensure_destination_not_on_disk, ensure_report_matches_disk, list_local_disks,
    plan_recovery_destination,
};
#[cfg(target_os = "macos")]
pub use helper::run as run_privileged_helper;
pub use privilege::{ScanSession, privileged_preview, privileged_recover, privileged_scan_device};
pub use status::{SystemStatus, detect_system_status};

#[cfg(test)]
mod tests;
