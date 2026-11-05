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

//! FileBack 菜单栏与快捷键：动作定义及到主视图的转发。

use gpui::{App, Context, KeyBinding, Menu, MenuItem, actions};
use std::sync::OnceLock;

use super::FileBackApp;

actions!(
    fileback,
    [
        Quit,
        About,
        RefreshDisks,
        ChooseDestination,
        StartScan,
        TogglePause,
        CancelScan,
        StartRecovery,
        RevealOutput
    ]
);

/// 全局保存主视图句柄，菜单/快捷键动作通过它转发到界面
pub(crate) static APP_VIEW: OnceLock<gpui::WeakEntity<FileBackApp>> = OnceLock::new();

fn with_view(cx: &mut App, f: impl FnOnce(&mut FileBackApp, &mut Context<FileBackApp>)) {
    if let Some(view) = APP_VIEW.get() {
        view.update(cx, |this, cx| f(this, cx)).ok();
    }
}

/// 注册快捷键、菜单栏，并把动作绑定到主视图。
pub(crate) fn register(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-r", RefreshDisks, None),
        KeyBinding::new("cmd-o", ChooseDestination, None),
        KeyBinding::new("cmd-s", StartScan, None),
        KeyBinding::new("cmd-p", TogglePause, None),
        KeyBinding::new("cmd-.", CancelScan, None),
        KeyBinding::new("cmd-e", StartRecovery, None),
        KeyBinding::new("cmd-shift-o", RevealOutput, None),
    ]);
    cx.set_menus(vec![
        Menu {
            name: "FileBack".into(),
            items: vec![
                MenuItem::action("关于 FileBack", About),
                MenuItem::separator(),
                MenuItem::action("退出 FileBack", Quit),
            ],
        },
        Menu {
            name: "操作".into(),
            items: vec![
                MenuItem::action("刷新磁盘列表", RefreshDisks),
                MenuItem::action("开始扫描", StartScan),
                MenuItem::action("暂停 / 继续", TogglePause),
                MenuItem::action("取消扫描", CancelScan),
                MenuItem::separator(),
                MenuItem::action("选择恢复位置", ChooseDestination),
                MenuItem::action("恢复选中文件", StartRecovery),
                MenuItem::action("在 Finder 中显示", RevealOutput),
            ],
        },
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &About, cx| {
        with_view(cx, |this, cx| {
            this.status = "FileBack · 本地磁盘文件恢复工具 · 只读扫描 / 勾选恢复".into();
            cx.notify();
        })
    });
    cx.on_action(|_: &RefreshDisks, cx| with_view(cx, |this, cx| this.refresh_disks(cx)));
    cx.on_action(|_: &ChooseDestination, cx| with_view(cx, |this, cx| this.choose_destination(cx)));
    cx.on_action(|_: &StartScan, cx| with_view(cx, |this, cx| this.start_scan(cx)));
    cx.on_action(|_: &TogglePause, cx| with_view(cx, |this, cx| this.toggle_pause(cx)));
    cx.on_action(|_: &CancelScan, cx| with_view(cx, |this, cx| this.cancel_scan(cx)));
    cx.on_action(|_: &StartRecovery, cx| with_view(cx, |this, cx| this.start_recovery(cx)));
    cx.on_action(|_: &RevealOutput, cx| with_view(cx, |this, cx| this.reveal_output(cx)));
}
