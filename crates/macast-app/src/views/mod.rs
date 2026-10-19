//! The main window view.

use gpui::*;
use macast_core::{MacastCore, PlaybackSnapshot, TransportState};
use std::sync::Arc;

pub struct RootView {
    core: Arc<MacastCore>,
    snapshot: PlaybackSnapshot,
    notice: Option<String>,
}

impl RootView {
    pub fn new(core: Arc<MacastCore>, cx: &mut Context<Self>) -> Self {
        let mut rx = core.subscribe();
        let initial = rx.borrow_and_update().clone();

        // Watch channel -> view updates. `tokio::sync::watch` futures are
        // runtime-agnostic, so they play nicely with gpui's own executor.
        // gpui 0.2 (edition 2024) wants an async closure here.
        cx.spawn(async move |view, cx| {
            while rx.changed().await.is_ok() {
                let snap = rx.borrow_and_update().clone();
                if view
                    .update(&mut *cx, |this, cx| {
                        this.snapshot = snap;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        Self {
            core,
            snapshot: initial,
            notice: None,
        }
    }

    // ---- local control helpers ------------------------------------------
    // They spawn onto the core's tokio runtime; the core routes them through
    // the exact same handlers as remote DLNA control.

    fn spawn_core<F>(&self, f: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        self.core.handle().spawn(f);
    }

    fn play(&self) {
        let core = self.core.clone();
        self.spawn_core(async move { core.play().await });
    }

    fn pause(&self) {
        let core = self.core.clone();
        self.spawn_core(async move { core.pause().await });
    }

    fn stop(&self) {
        let core = self.core.clone();
        self.spawn_core(async move { core.stop().await });
    }

    fn nudge_volume(&self, delta: i16) {
        let target = (self.snapshot.volume as i16 + delta).clamp(0, 100) as u8;
        let core = self.core.clone();
        self.spawn_core(async move { core.set_volume(target).await });
    }

    fn toggle_mute(&self) {
        let mute = !self.snapshot.mute;
        let core = self.core.clone();
        self.spawn_core(async move { core.set_mute(mute).await });
    }

    fn copy_link(&mut self, cx: &mut Context<Self>) {
        let uri = self
            .core
            .engine
            .state
            .try_read()
            .map(|g| g.get("CurrentTrackURI"))
            .unwrap_or_default();
        self.notice = Some(if uri.is_empty() {
            "当前没有可复制的链接".to_string()
        } else {
            // arboard needs a thread with a message loop; a quick blocking call
            // on the UI thread is fine for a one-shot clipboard write.
            match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(uri)) {
                Ok(()) => "链接已复制到剪贴板".to_string(),
                Err(e) => format!("复制失败: {e}"),
            }
        });
        cx.notify();
    }
}

impl Render for RootView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = self.snapshot.clone();
        let playing = s.transport_state == TransportState::Playing;
        let has_media = !s.uri.is_empty();
        let frac = if s.duration > 0.0 {
            (s.position / s.duration).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        let vol_frac = (s.volume as f32 / 100.0).clamp(0.0, 1.0);

        let mut root = div()
            .id("root-scroll")
            .size_full()
            .bg(rgb(0xF5F6F8))
            .text_color(rgb(0x1F2328))
            .flex()
            .flex_col()
            .p_6()
            .gap_5()
            // `overflow_y_scroll` lives on `StatefulInteractiveElement`, so the
            // div needs an id first (gpui 0.2.2 has no unscoped scroll helper).
            .overflow_y_scroll()
            // ---- header -------------------------------------------------
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::BOLD)
                                    .child("Macast-Gpui"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(0x6B7280))
                                    .child(format!(
                                        "DLNA 媒体接收端 · {}",
                                        if s.friendly_name.is_empty() {
                                            "…"
                                        } else {
                                            s.friendly_name.as_str()
                                        }
                                    )),
                            ),
                    )
                    .child(state_badge(&s)),
            )
            // ---- now playing --------------------------------------------
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_5()
                    .rounded_xl()
                    .bg(rgb(0xFFFFFF))
                    .border_1()
                    .border_color(rgb(0xE5E7EB))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(if has_media {
                                s.title.clone()
                            } else {
                                "等待投屏…".to_string()
                            }),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(0x6B7280))
                            .child(if has_media {
                                s.uri.clone()
                            } else {
                                "在手机上选择“投屏 / Cast”，然后选择本设备".to_string()
                            }),
                    )
                    .child(
                        div()
                            .mt_2()
                            .h(px(8.0))
                            .w_full()
                            .rounded_full()
                            .bg(rgb(0xE5E7EB))
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(frac))
                                    .rounded_full()
                                    .bg(rgb(0x2F6FEB)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .text_xs()
                            .text_color(rgb(0x6B7280))
                            .child(fmt_time(s.position))
                            .child(fmt_time(s.duration)),
                    ),
            )
            // ---- transport controls -------------------------------------
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(button(
                        "play-pause",
                        if playing { "暂停" } else { "播放" },
                        cx.listener(|this, _, _, _| {
                            if this.snapshot.transport_state == TransportState::Playing {
                                this.pause();
                            } else {
                                this.play();
                            }
                        }),
                    ))
                    .child(button(
                        "stop",
                        "停止",
                        cx.listener(|this, _, _, _| this.stop()),
                    ))
                    .child(button(
                        "mute",
                        if s.mute { "取消静音" } else { "静音" },
                        cx.listener(|this, _, _, _| this.toggle_mute()),
                    ))
                    .child(button(
                        "copy-link",
                        "复制链接",
                        cx.listener(|this, _, _, cx| this.copy_link(cx)),
                    ))
                    .child(div().flex_1())
                    .child(button(
                        "vol-down",
                        "−",
                        cx.listener(|this, _, _, _| this.nudge_volume(-10)),
                    ))
                    .child(
                        div()
                            .w(px(120.0))
                            .h(px(8.0))
                            .rounded_full()
                            .bg(rgb(0xE5E7EB))
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(vol_frac))
                                    .rounded_full()
                                    .bg(rgb(0x10B981)),
                            ),
                    )
                    .child(button(
                        "vol-up",
                        "+",
                        cx.listener(|this, _, _, _| this.nudge_volume(10)),
                    ))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(0x6B7280))
                            .child(format!("{}%", s.volume)),
                    ),
            )
            // ---- network info -------------------------------------------
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_5()
                    .rounded_xl()
                    .bg(rgb(0xFFFFFF))
                    .border_1()
                    .border_color(rgb(0xE5E7EB))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("网络信息"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(0x6B7280))
                            .child(format!(
                                "端口: {}   ·   播放器: {}",
                                s.port,
                                self.core.engine.player.name()
                            )),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(0x6B7280))
                            .child(if s.ips.is_empty() {
                                "未找到可用网卡".to_string()
                            } else {
                                format!("地址: {}", s.ips.join(", "))
                            }),
                    ),
            );

        if let Some(notice) = &self.notice {
            root = root.child(
                div()
                    .text_sm()
                    .text_color(rgb(0x2F6FEB))
                    .child(notice.clone()),
            );
        }

        root.child(
            div()
                .text_xs()
                .text_color(rgb(0x9CA3AF))
                .child(format!(
                    "状态: {}   ·   详细日志见控制台",
                    s.transport_state.as_str()
                )),
        )
    }
}

/// gpui 0.2.2 (crates.io build) ships no `Button` component, so we roll a
/// small one out of a clickable `div`. Returns `Stateful<Div>` because
/// `on_click` lives on `StatefulInteractiveElement`.
fn button(
    id: &'static str,
    label: impl IntoElement + 'static,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .px_4()
        .py_2()
        .rounded_md()
        .bg(rgb(0x2F6FEB))
        .hover(|s| s.bg(rgb(0x2563EB)))
        .active(|s| s.bg(rgb(0x1D4ED8)))
        .text_color(rgb(0xFFFFFF))
        .text_sm()
        .cursor_pointer()
        .child(label)
        .on_click(on_click)
}

fn state_badge(s: &PlaybackSnapshot) -> Div {
    let (label, color) = match s.transport_state {
        TransportState::Playing => ("播放中", 0x10B981u32),
        TransportState::PausedPlayback => ("已暂停", 0xF59E0B),
        TransportState::Stopped => ("已停止", 0x6B7280),
        TransportState::NoMediaPresent => ("空闲", 0x9CA3AF),
    };
    div()
        .px_3()
        .py_1()
        .rounded_full()
        .bg(rgb(color))
        .text_color(rgb(0xFFFFFF))
        .text_sm()
        .child(label)
}

fn fmt_time(secs: f64) -> String {
    let t = secs.round().max(0.0) as u64;
    format!("{:02}:{:02}", t / 60, t % 60)
}
