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
pub use privilege::{
    ScanSession, privileged_analyze, privileged_preview, privileged_recover, privileged_scan_device,
};
pub use status::{SystemStatus, detect_system_status};

#[cfg(test)]
mod tests;
