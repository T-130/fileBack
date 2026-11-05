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

use std::process::Command;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemStatus {
    pub local_snapshots: Option<usize>,
    pub time_machine_configured: bool,
}

impl SystemStatus {
    pub fn summary(&self) -> String {
        let snapshots = match self.local_snapshots {
            Some(count) => format!("APFS 本地快照：{count} 个"),
            None => "APFS 本地快照：检测失败".into(),
        };
        let backup = if self.time_machine_configured {
            "Time Machine：已配置备份目标"
        } else {
            "Time Machine：未检测到备份目标"
        };
        format!("{snapshots} · {backup}")
    }
}

pub fn detect_system_status() -> SystemStatus {
    let snapshots = Command::new("tmutil")
        .args(["listlocalsnapshots", "/"])
        .output();
    let local_snapshots = match snapshots {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            Some(
                text.lines()
                    .filter(|line| line.trim_start().starts_with("com.apple."))
                    .count(),
            )
        }
        _ => None,
    };

    let time_machine_configured = Command::new("tmutil")
        .arg("destinationinfo")
        .output()
        .is_ok_and(|output| output.status.success());

    SystemStatus {
        local_snapshots,
        time_machine_configured,
    }
}
