//! FileBack 图形界面模块（gpui）。
//!
//! - [`actions`]：菜单栏与快捷键动作
//! - [`state`]：界面状态与扫描/恢复业务流程
//! - [`view`]：渲染层

mod actions;
mod state;
mod view;

pub(crate) use state::FileBackApp;

use gpui::{App, Application, Bounds, WindowBounds, WindowOptions, prelude::*, px, size};

/// 启动图形界面。
pub fn run() {
    Application::new().run(|cx: &mut App| {
        actions::register(cx);

        let bounds = Bounds::centered(None, size(px(900.0), px(760.0)), cx);
        match cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| FileBackApp::new()),
        ) {
            Ok(handle) => {
                if let Ok(view) = handle.entity(cx) {
                    let _ = actions::APP_VIEW.set(view.downgrade());
                    view.update(cx, |app, cx| app.initialize(cx));
                }
            }
            Err(error) => {
                eprintln!("无法创建 FileBack 窗口：{error}");
                return;
            }
        }
        cx.activate(true);
    });
}

pub(crate) fn format_size(size: u64) -> String {
    const GB: u64 = 1024 * 1024 * 1024;
    const MB: u64 = 1024 * 1024;
    if size >= GB {
        format!("{:.1} GB", size as f64 / GB as f64)
    } else if size >= MB {
        format!("{:.1} MB", size as f64 / MB as f64)
    } else if size >= 1024 {
        format!("{:.1} KB", size as f64 / 1024.0)
    } else {
        format!("{size} B")
    }
}
