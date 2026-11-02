//! FileBack 界面渲染：按磁盘、扫描、候选、恢复四个阶段组织操作。

use gpui::{
    Context, FontWeight, Image, ImageFormat, ObjectFit, Window, div, img, prelude::*, px, rgb,
};
use std::sync::Arc;

use crate::{device::DeviceCandidate, recovery::FileCategory};

use super::{FileBackApp, format_size, state::CANDIDATES_PER_PAGE};

impl Render for FileBackApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let disks = self.disks.clone();
        let selected_disk = self.selected_disk.clone();
        let selected_name = self
            .selected_disk
            .as_ref()
            .and_then(|identifier| {
                self.disks
                    .iter()
                    .find(|disk| &disk.identifier == identifier)
            })
            .map(|disk| format!("{} · {}", disk.name, disk.raw_device.display()))
            .unwrap_or_else(|| "尚未选择".into());
        let scan_report = self.scan_report.clone();
        let recovery_report = self.recovery_report.clone();
        let selected_candidates = self.selected_candidates.clone();
        let selected_count = selected_candidates.len();
        let live_candidates: Vec<DeviceCandidate> =
            self.live_candidates.iter().rev().take(5).cloned().collect();
        let live_total = self.live_candidates.len();
        let destination = self
            .destination
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "尚未选择（请选择另一块物理磁盘上的位置）".into());
        let busy = self.busy();

        div()
            .id("fileback-main-scroll")
            .size_full()
            .overflow_y_scroll()
            .bg(rgb(0xf8fafc))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .p_6()
                    .text_color(rgb(0x172033))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .items_end()
                            .gap_4()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_2xl()
                                            .font_weight(FontWeight::BOLD)
                                            .child("FileBack"),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(rgb(0x64748b))
                                            .child("只读扫描，确认候选，再恢复到另一块磁盘"),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(0x64748b))
                                    .flex_shrink_0()
                                    .child("本地处理 · 不上传数据"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .p_4()
                            .rounded_lg()
                            .bg(rgb(0xfff7e8))
                            .border_1()
                            .border_color(rgb(0xf2c46d))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("先保护源数据"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .whitespace_normal()
                                    .child("不要恢复到源磁盘。签名雕刻不能还原完整目录结构，也无法保证文件可用；重要数据请同时使用其他恢复方案。"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(0x72531b))
                                    .child(self.system_status.clone()),
                            ),
                    )
                    .child(
                        div()
                            .p_3()
                            .rounded_md()
                            .bg(rgb(0xeaf0f8))
                            .border_1()
                            .border_color(rgb(0xd8e2ef))
                            .text_sm()
                            .whitespace_normal()
                            .child(self.status.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .p_4()
                            .rounded_lg()
                            .bg(rgb(0xffffff))
                            .border_1()
                            .border_color(rgb(0xdce4ef))
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .items_center()
                                    .child(
                                        div()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child("1. 选择物理磁盘"),
                                    )
                                    .child(
                                        div()
                                            .id("refresh-disks")
                                            .text_sm()
                                            .text_color(if busy {
                                                rgb(0x94a3b8)
                                            } else {
                                                rgb(0x2563eb)
                                            })
                                            .child("刷新列表")
                                            .when(!busy, |button| {
                                                button.cursor_pointer().on_click(
                                                    cx.listener(|this, _, _, cx| {
                                                        this.refresh_disks(cx)
                                                    }),
                                                )
                                            }),
                                    ),
                            )
                            .when(disks.is_empty(), |section| {
                                section.child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(0x64748b))
                                        .child("没有检测到支持扫描的物理磁盘，请连接设备后刷新。"),
                                )
                            })
                            .children(disks.into_iter().enumerate().map(|(index, disk)| {
                                let identifier = disk.identifier.clone();
                                let selected = selected_disk.as_ref() == Some(&disk.identifier);
                                div()
                                    .id(index)
                                    .flex()
                                    .justify_between()
                                    .items_center()
                                    .gap_4()
                                    .p_3()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(if selected {
                                        rgb(0x2563eb)
                                    } else {
                                        rgb(0xdce4ef)
                                    })
                                    .bg(if selected {
                                        rgb(0xeaf2ff)
                                    } else {
                                        rgb(0xffffff)
                                    })
                                    .child(
                                        div()
                                            .flex_1()
                                            .truncate()
                                            .child(format!(
                                                "{} · {} · {}",
                                                disk.name,
                                                disk.identifier,
                                                if disk.internal { "内置" } else { "外接" }
                                            )),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .flex_shrink_0()
                                            .child(format_size(disk.size)),
                                    )
                                    .when(!busy, |row| {
                                        row.cursor_pointer().on_click(cx.listener(
                                            move |this, _, _, cx| {
                                                this.select_disk(&identifier, cx)
                                            },
                                        ))
                                    })
                            }))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(0x64748b))
                                    .truncate()
                                    .child(format!("源设备：{selected_name}")),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .p_4()
                            .rounded_lg()
                            .bg(rgb(0xffffff))
                            .border_1()
                            .border_color(rgb(0xdce4ef))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("2. 只读扫描"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(0x64748b))
                                    .whitespace_normal()
                                    .child("首次使用需管理员确认安装受限磁盘助手，并为 FileBack 及 FileBack.helper 开启完全磁盘访问权限。扫描不会写入恢复文件。"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap_3()
                                    .items_center()
                                    .child(
                                        div()
                                            .id("start-scan")
                                            .px_4()
                                            .py_2()
                                            .rounded_md()
                                            .bg(if busy {
                                                rgb(0x94a3b8)
                                            } else {
                                                rgb(0x2563eb)
                                            })
                                            .text_color(rgb(0xffffff))
                                            .child(if self.scanning {
                                                "扫描中……"
                                            } else {
                                                "开始只读扫描"
                                            })
                                            .when(!busy, |button| {
                                                button.cursor_pointer().on_click(
                                                    cx.listener(|this, _, _, cx| {
                                                        this.start_scan(cx)
                                                    }),
                                                )
                                            }),
                                    )
                                    .when(self.scanning, |controls| {
                                        controls
                                            .child(
                                                div()
                                                    .id("toggle-pause")
                                                    .px_4()
                                                    .py_2()
                                                    .rounded_md()
                                                    .bg(rgb(0xffffff))
                                                    .border_1()
                                                    .border_color(rgb(0xcbd5e1))
                                                    .cursor_pointer()
                                                    .child(if self.scan_paused {
                                                        "继续扫描"
                                                    } else {
                                                        "暂停扫描"
                                                    })
                                                    .on_click(cx.listener(
                                                        |this, _, _, cx| this.toggle_pause(cx),
                                                    )),
                                            )
                                            .child(
                                                div()
                                                    .id("cancel-scan")
                                                    .px_4()
                                                    .py_2()
                                                    .rounded_md()
                                                    .bg(rgb(0xffffff))
                                                    .border_1()
                                                    .border_color(rgb(0xdc2626))
                                                    .text_color(rgb(0xdc2626))
                                                    .cursor_pointer()
                                                    .child("取消扫描")
                                                    .on_click(cx.listener(
                                                        |this, _, _, cx| this.cancel_scan(cx),
                                                    )),
                                            )
                                    }),
                            ),
                    )
                    .when(self.scanning && live_total > 0, |container| {
                        container.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .p_4()
                                .rounded_lg()
                                .bg(rgb(0xffffff))
                                .border_1()
                                .border_color(rgb(0xdce4ef))
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(format!(
                                            "实时发现（共 {live_total} 个，最新 5 个）"
                                        )),
                                )
                                .children(live_candidates.into_iter().map(|candidate| {
                                    div()
                                        .flex()
                                        .justify_between()
                                        .gap_4()
                                        .text_sm()
                                        .child(format!(
                                            "{} · {} · {}",
                                            candidate.kind.category().label(),
                                            candidate.kind.label(),
                                            format_size(candidate.size)
                                        ))
                                        .child(
                                            div()
                                                .flex_shrink_0()
                                                .child(format!("源偏移 {}", candidate.source_offset)),
                                        )
                                })),
                        )
                    })
                    .when_some(scan_report, |container, report| {
                        let total = report.candidates.len();
                        let page_count = total.div_ceil(CANDIDATES_PER_PAGE);
                        let page = self
                            .candidate_page
                            .min(page_count.saturating_sub(1));
                        let start = page * CANDIDATES_PER_PAGE;
                        let shown: Vec<DeviceCandidate> = report
                            .candidates
                            .iter()
                            .skip(start)
                            .take(CANDIDATES_PER_PAGE)
                            .cloned()
                            .collect();
                        let page_selected = !shown.is_empty()
                            && shown
                                .iter()
                                .all(|candidate| selected_candidates.contains(&candidate.id));

                        container.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .p_4()
                                .rounded_lg()
                                .bg(rgb(0xffffff))
                                .border_1()
                                .border_color(rgb(0xdce4ef))
                                .child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .justify_between()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child("3. 选择候选文件"),
                                        )
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(rgb(0x64748b))
                                                .child(format!(
                                                    "共 {total} 个 · 已选 {selected_count} 个"
                                                )),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(0x64748b))
                                        .whitespace_normal()
                                        .child("原文件名通常无法从签名恢复；请结合缩略图、尺寸或文档标题选择。自动编号仅用于防止覆盖。"),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .gap_3()
                                        .text_xs()
                                        .text_color(rgb(0x64748b))
                                        .children(FileCategory::ALL.into_iter().filter_map(|category| {
                                            let count = report
                                                .candidates
                                                .iter()
                                                .filter(|candidate| candidate.kind.category() == category)
                                                .count();
                                            (count > 0).then(|| format!("{} {count}", category.label()))
                                        })),
                                )
                                .when_some(self.preview_error.clone(), |section, error| {
                                    section.child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .gap_3()
                                            .p_2()
                                            .rounded_md()
                                            .bg(rgb(0xfff7e8))
                                            .text_sm()
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .whitespace_normal()
                                                    .child(error),
                                            )
                                            .child(
                                                div()
                                                    .id("retry-candidate-previews")
                                                    .flex_shrink_0()
                                                    .text_color(rgb(0x2563eb))
                                                    .child("重试预览")
                                                    .when(!busy, |button| {
                                                        button.cursor_pointer().on_click(cx.listener(
                                                            |this, _, _, cx| {
                                                                this.load_candidate_previews(cx)
                                                            },
                                                        ))
                                                    }),
                                            ),
                                    )
                                })
                                .when(report.canceled, |section| {
                                    section.child(
                                        div()
                                            .p_2()
                                            .rounded_md()
                                            .bg(rgb(0xfff7e8))
                                            .text_sm()
                                            .child("本次扫描已取消，以下只是部分结果。"),
                                    )
                                })
                                .when(total == 0, |section| {
                                    section.child(
                                        div()
                                            .py_4()
                                            .text_sm()
                                            .text_color(rgb(0x64748b))
                                            .child("没有找到完整候选文件。"),
                                    )
                                })
                                .children(shown.into_iter().enumerate().map(|(index, candidate)| {
                                    let id = candidate.id.clone();
                                    let kind = candidate.kind;
                                    let checked = selected_candidates.contains(&id);
                                    let preview = self.candidate_previews.get(&id).cloned();
                                    let thumbnail = preview
                                        .as_ref()
                                        .and_then(|preview| preview.thumbnail_png.clone());
                                    let loading = preview.is_none() && self.preview_loading;
                                    let title = preview
                                        .as_ref()
                                        .and_then(|preview| preview.title.clone())
                                        .map(|title| format!("标题：{title}"))
                                        .unwrap_or_else(|| format!("{} · 自动命名", kind.label()));
                                    let details = preview
                                        .as_ref()
                                        .map(|preview| preview.details.join(" · "))
                                        .unwrap_or_default();
                                    let item_error = preview
                                        .as_ref()
                                        .and_then(|preview| preview.error.clone());
                                    div()
                                        .id(("candidate", start + index))
                                        .flex()
                                        .items_center()
                                        .gap_3()
                                        .p_3()
                                        .rounded_md()
                                        .bg(if checked {
                                            rgb(0xeaf2ff)
                                        } else {
                                            rgb(0xffffff)
                                        })
                                        .border_1()
                                        .border_color(if checked {
                                            rgb(0x2563eb)
                                        } else {
                                            rgb(0xe2e8f0)
                                        })
                                        .child(
                                            div()
                                                .flex_shrink_0()
                                                .child(if checked { "☑" } else { "☐" }),
                                        )
                                        .child(
                                            div()
                                                .w(px(112.0))
                                                .h(px(76.0))
                                                .flex_shrink_0()
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .overflow_hidden()
                                                .rounded_md()
                                                .bg(rgb(0xeaf0f8))
                                                .text_xs()
                                                .text_color(rgb(0x64748b))
                                                .when_some(thumbnail, |container, bytes| {
                                                    container.child(
                                                        img(Arc::new(Image::from_bytes(
                                                            ImageFormat::Png,
                                                            bytes,
                                                        )))
                                                        .size_full()
                                                        .object_fit(ObjectFit::Cover),
                                                    )
                                                })
                                                .when(preview.as_ref().is_none_or(|preview| {
                                                    preview.thumbnail_png.is_none()
                                                }), |container| {
                                                    container.child(if loading {
                                                        "加载中…"
                                                    } else {
                                                        kind.label()
                                                    })
                                                }),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .flex()
                                                .flex_col()
                                                .gap_1()
                                                .child(
                                                    div()
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .truncate()
                                                        .child(title),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(rgb(0x64748b))
                                                        .child(format!(
                                                            "{} · {} · 源偏移 {}",
                                                            candidate.id,
                                                            kind.category().label(),
                                                            candidate.source_offset
                                                        )),
                                                )
                                                .when(!details.is_empty(), |container| {
                                                    container.child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(rgb(0x475569))
                                                            .child(details),
                                                    )
                                                })
                                                .when_some(item_error, |container, error| {
                                                    container.child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(rgb(0xa16207))
                                                            .child(error),
                                                    )
                                                }),
                                        )
                                        .child(
                                            div()
                                                .text_sm()
                                                .flex_shrink_0()
                                                .child(format_size(candidate.size)),
                                        )
                                        .when(!busy, |row| {
                                            row.cursor_pointer().on_click(cx.listener(
                                                move |this, _, _, cx| {
                                                    this.toggle_candidate(&id, cx)
                                                },
                                            ))
                                        })
                                }))
                                .when(total > 0, |section| {
                                    section.child(
                                        div()
                                            .flex()
                                            .flex_wrap()
                                            .items_center()
                                            .gap_3()
                                            .pt_2()
                                            .child(
                                                div()
                                                    .id("select-candidate-page")
                                                    .px_3()
                                                    .py_2()
                                                    .rounded_md()
                                                    .border_1()
                                                    .border_color(rgb(0xcbd5e1))
                                                    .text_sm()
                                                    .cursor_pointer()
                                                    .child(if page_selected {
                                                        "取消选择本页"
                                                    } else {
                                                        "选择本页"
                                                    })
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.toggle_page_selection(cx)
                                                    })),
                                            )
                                            .child(
                                                div()
                                                    .id("previous-candidate-page")
                                                    .px_3()
                                                    .py_2()
                                                    .rounded_md()
                                                    .border_1()
                                                    .border_color(rgb(0xcbd5e1))
                                                    .text_sm()
                                                    .text_color(if page == 0 {
                                                        rgb(0x94a3b8)
                                                    } else {
                                                        rgb(0x172033)
                                                    })
                                                    .child("上一页")
                                                    .when(page > 0 && !busy, |button| {
                                                        button.cursor_pointer().on_click(
                                                            cx.listener(|this, _, _, cx| {
                                                                this.previous_candidate_page(cx)
                                                            }),
                                                        )
                                                    }),
                                            )
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .text_color(rgb(0x64748b))
                                                    .child(format!(
                                                        "第 {} / {} 页",
                                                        page + 1,
                                                        page_count.max(1)
                                                    )),
                                            )
                                            .child(
                                                div()
                                                    .id("next-candidate-page")
                                                    .px_3()
                                                    .py_2()
                                                    .rounded_md()
                                                    .border_1()
                                                    .border_color(rgb(0xcbd5e1))
                                                    .text_sm()
                                                    .text_color(if page + 1 >= page_count {
                                                        rgb(0x94a3b8)
                                                    } else {
                                                        rgb(0x172033)
                                                    })
                                                    .child("下一页")
                                                    .when(page + 1 < page_count && !busy, |button| {
                                                        button.cursor_pointer().on_click(
                                                            cx.listener(|this, _, _, cx| {
                                                                this.next_candidate_page(cx)
                                                            }),
                                                        )
                                                    }),
                                            ),
                                    )
                                }),
                        )
                    })
                    .when(self.scan_report.is_some(), |container| {
                        let recovery_ready =
                            !busy && selected_count > 0 && self.destination.is_some();
                        container.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .p_4()
                                .rounded_lg()
                                .bg(rgb(0xffffff))
                                .border_1()
                                .border_color(rgb(0xdce4ef))
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("4. 选择恢复位置"),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(0x64748b))
                                        .whitespace_normal()
                                        .child("选择另一块磁盘或文件夹，确认后自动新建独立目录；已有文件不会被覆盖。"),
                                )
                                .child(
                                    div()
                                        .w_full()
                                        .text_sm()
                                        .whitespace_normal()
                                        .child(format!("恢复目标：{destination}")),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .gap_3()
                                        .child(
                                            div()
                                                .id("choose-destination")
                                                .px_4()
                                                .py_2()
                                                .rounded_md()
                                                .bg(rgb(0xffffff))
                                                .border_1()
                                                .border_color(rgb(0xcbd5e1))
                                                .text_color(if busy {
                                                    rgb(0x94a3b8)
                                                } else {
                                                    rgb(0x172033)
                                                })
                                                .child(if self.choosing_destination {
                                                    "正在选择恢复位置…"
                                                } else {
                                                    "选择恢复位置"
                                                })
                                                .when(!busy, |button| {
                                                    button.cursor_pointer().on_click(
                                                        cx.listener(|this, _, _, cx| {
                                                            this.choose_destination(cx)
                                                        }),
                                                    )
                                                }),
                                        )
                                        .child(
                                            div()
                                                .id("start-recovery")
                                                .px_4()
                                                .py_2()
                                                .rounded_md()
                                                .bg(if recovery_ready {
                                                    rgb(0x16a34a)
                                                } else {
                                                    rgb(0x94a3b8)
                                                })
                                                .text_color(rgb(0xffffff))
                                                .child(if self.recovering {
                                                    "恢复中……"
                                                } else {
                                                    "确认并恢复选中文件"
                                                })
                                                .when(recovery_ready, |button| {
                                                    button.cursor_pointer().on_click(
                                                        cx.listener(|this, _, _, cx| {
                                                            this.start_recovery(cx)
                                                        }),
                                                    )
                                                }),
                                        ),
                                )
                                .child(
                                    div()
                                        .p_3()
                                        .rounded_md()
                                        .bg(rgb(0xeaf0f8))
                                        .text_sm()
                                        .whitespace_normal()
                                        .child(self.status.clone()),
                                ),
                        )
                    })
                    .when_some(recovery_report, |container, report| {
                        container.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .p_4()
                                .rounded_lg()
                                .bg(rgb(0xffffff))
                                .border_1()
                                .border_color(rgb(0xdce4ef))
                                .child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .justify_between()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(format!(
                                                    "恢复结果（共 {} 个）",
                                                    report.files.len()
                                                )),
                                        )
                                        .child(
                                            div()
                                                .id("reveal-output")
                                                .text_sm()
                                                .text_color(rgb(0x2563eb))
                                                .cursor_pointer()
                                                .child("在 Finder 中显示")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.reveal_output(cx)
                                                })),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(0x64748b))
                                        .whitespace_normal()
                                        .child(format!(
                                            "输出目录：{}",
                                            report.output_dir.display()
                                        )),
                                )
                                .children(report.files.iter().take(8).map(|file| {
                                    div()
                                        .flex()
                                        .justify_between()
                                        .gap_4()
                                        .text_sm()
                                        .child(format!(
                                            "{} · {}",
                                            file.path
                                                .file_name()
                                                .unwrap_or_default()
                                                .to_string_lossy(),
                                            format_size(file.size)
                                        ))
                                        .child(
                                            div().flex_shrink_0().child(format!(
                                                "{} · {}",
                                                file.kind.category().label(),
                                                file.kind.label()
                                            )),
                                        )
                                }))
                                .when(report.files.len() > 8, |section| {
                                    section.child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(0x64748b))
                                            .child("仅显示前 8 个，完整文件请前往输出目录查看。"),
                                    )
                                }),
                        )
                    }),
            )
    }
}
