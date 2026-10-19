//! The main window view.

use gpui::*;
use macast_core::{Config, MacastCore, PlaybackSnapshot, PlayerKind, TransportState};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

pub struct RootView {
    core: Arc<MacastCore>,
    /// Config as last loaded/saved by the UI. `core.engine.config` is frozen at
    /// startup, so edits are written to disk and applied on the next launch.
    config: Config,
    snapshot: PlaybackSnapshot,
    notice: Option<String>,
    /// Incremented whenever a notice is set, so stale timers do not clear it.
    notice_seq: u64,
    show_settings: bool,
    /// Whether the app is registered to start with the OS session.
    autostart: bool,
    /// Bounds of the seek bar, captured while painting it. Click coordinates are
    /// window-relative, so we need the bar's own rectangle to compute a fraction.
    seek_bounds: Rc<Cell<Bounds<Pixels>>>,
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

        let config = core.engine.config.clone();
        let autostart = is_autostart_enabled();

        Self {
            core,
            config,
            snapshot: initial,
            notice: None,
            notice_seq: 0,
            show_settings: false,
            autostart,
            seek_bounds: Rc::new(Cell::new(Bounds::default())),
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

    /// Seek by a fraction (0..=1) of the current media duration.
    fn seek_to(&self, fraction: f64) {
        if self.snapshot.duration <= 0.0 {
            return;
        }
        let target = (fraction * self.snapshot.duration).clamp(0.0, self.snapshot.duration);
        let core = self.core.clone();
        self.spawn_core(async move { core.seek(target).await });
    }

    fn nudge_position(&self, delta: f64) {
        if self.snapshot.duration <= 0.0 && self.snapshot.position <= 0.0 {
            return;
        }
        let target = (self.snapshot.position + delta).max(0.0);
        let target = if self.snapshot.duration > 0.0 {
            target.min(self.snapshot.duration)
        } else {
            target
        };
        let core = self.core.clone();
        self.spawn_core(async move { core.seek(target).await });
    }

    /// Play a local file dropped onto the window.
    fn play_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let label = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        match url::Url::from_file_path(&path) {
            Ok(url) => {
                let title = path.file_stem().map(|s| s.to_string_lossy().to_string());
                let core = self.core.clone();
                self.spawn_core(async move { core.play_uri(url.to_string(), title).await });
                self.set_notice(format!("正在播放：{label}"), cx);
            }
            Err(_) => self.set_notice(format!("无法把路径转成 URI：{}", path.display()), cx),
        }
    }

    fn copy_link(&mut self, cx: &mut Context<Self>) {
        let uri = self
            .core
            .engine
            .state
            .try_read()
            .map(|g| g.get("CurrentTrackURI"))
            .unwrap_or_default();
        let message = if uri.is_empty() {
            "当前没有可复制的链接".to_string()
        } else {
            // arboard needs a thread with a message loop; a quick blocking call
            // on the UI thread is fine for a one-shot clipboard write.
            match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(uri)) {
                Ok(()) => "链接已复制到剪贴板".to_string(),
                Err(e) => format!("复制失败: {e}"),
            }
        };
        self.set_notice(message, cx);
    }

    /// Persist `self.config`, then tell the user a restart is required.
    fn save_config(&mut self, what: &str, cx: &mut Context<Self>) {
        match self.config.save() {
            Ok(()) => self.set_notice(format!("{what}已保存，重启应用后生效"), cx),
            Err(e) => self.set_notice(format!("保存配置失败：{e}"), cx),
        }
    }

    fn toggle_player(&mut self, cx: &mut Context<Self>) {
        self.config.player = match self.config.player {
            PlayerKind::Mpv => PlayerKind::Null,
            PlayerKind::Null => PlayerKind::Mpv,
        };
        self.save_config("播放后端", cx);
    }

    fn open_config(&mut self, cx: &mut Context<Self>) {
        // Make sure the file exists before handing it to the OS.
        if let Err(e) = self.config.save() {
            self.set_notice(format!("保存配置失败：{e}"), cx);
            return;
        }
        match open_in_default_app(&Config::config_path()) {
            Ok(()) => self.set_notice("已用系统默认程序打开 config.json", cx),
            Err(e) => self.set_notice(format!("无法打开配置文件：{e}"), cx),
        }
    }

    fn toggle_autostart(&mut self, cx: &mut Context<Self>) {
        let enable = !self.autostart;
        match set_autostart(enable) {
            Ok(()) => {
                self.autostart = enable;
                self.set_notice(
                    if enable {
                        "已开启开机自启"
                    } else {
                        "已关闭开机自启"
                    },
                    cx,
                );
            }
            Err(e) => self.set_notice(format!("设置开机自启失败：{e}"), cx),
        }
    }

    /// Show a transient message; it clears itself after a few seconds.
    fn set_notice(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.notice = Some(message.into());
        self.notice_seq = self.notice_seq.wrapping_add(1);
        let seq = self.notice_seq;
        cx.spawn(async move |view, cx| {
            cx.background_executor().timer(Duration::from_secs(4)).await;
            let _ = view.update(&mut *cx, |this, cx| {
                if this.notice_seq == seq {
                    this.notice = None;
                    cx.notify();
                }
            });
        })
        .detach();
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

        // The canvas paints the bar and records its bounds for click-to-seek.
        let seek_cell = self.seek_bounds.clone();
        let seek_bar = div()
            .id("seek-bar")
            .h(px(10.0))
            .w_full()
            .rounded_full()
            .overflow_hidden()
            .cursor_pointer()
            .on_click(cx.listener(|this, event: &ClickEvent, _, _| {
                let bounds = this.seek_bounds.get();
                if bounds.size.width > px(1.0) {
                    let fraction = ((event.position().x - bounds.origin.x) / bounds.size.width)
                        .clamp(0.0, 1.0) as f64;
                    this.seek_to(fraction);
                }
            }))
            .child(
                canvas(
                    move |bounds, _, _| {
                        seek_cell.set(bounds);
                    },
                    move |bounds, _, window, _| {
                        window.paint_quad(fill(bounds, rgb(0xE5E7EB)));
                        if frac > 0.0 {
                            let filled = Bounds::new(
                                bounds.origin,
                                size(bounds.size.width * frac, bounds.size.height),
                            );
                            window.paint_quad(fill(filled, rgb(0x2F6FEB)));
                        }
                    },
                )
                .h_full()
                .w_full(),
            );

        let mut root =
            div()
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
                // Dropping a local media file plays it directly. gpui maps OS file
                // drops onto its internal drag payload (`ExternalPaths`).
                .on_drop(cx.listener(|this, paths: &ExternalPaths, _window, cx| {
                    if let Some(path) = paths.paths().first() {
                        this.play_path(path.clone(), cx);
                    }
                }))
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
                                .child(div().text_sm().text_color(rgb(0x6B7280)).child(format!(
                                    "DLNA 媒体接收端 · {}",
                                    if s.friendly_name.is_empty() {
                                        "…"
                                    } else {
                                        s.friendly_name.as_str()
                                    }
                                ))),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(state_badge(&s))
                                .child(ghost_button(
                                    "toggle-settings",
                                    if self.show_settings {
                                        "收起设置"
                                    } else {
                                        "设置"
                                    },
                                    cx.listener(|this, _, _, cx| {
                                        this.show_settings = !this.show_settings;
                                        cx.notify();
                                    }),
                                )),
                        ),
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
                        .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(
                            if has_media {
                                s.title.clone()
                            } else {
                                "等待投屏…".to_string()
                            },
                        ))
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(0x6B7280))
                                .child(if has_media {
                                    s.uri.clone()
                                } else {
                                    "在手机上选择“投屏 / Cast”，或把本地视频拖进这个窗口"
                                        .to_string()
                                }),
                        )
                        .child(div().mt_2().child(seek_bar))
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
                        .child(ghost_button(
                            "seek-back",
                            "−10s",
                            cx.listener(|this, _, _, _| this.nudge_position(-10.0)),
                        ))
                        .child(ghost_button(
                            "seek-fwd",
                            "+10s",
                            cx.listener(|this, _, _, _| this.nudge_position(10.0)),
                        ))
                        .child(ghost_button(
                            "mute",
                            if s.mute { "取消静音" } else { "静音" },
                            cx.listener(|this, _, _, _| this.toggle_mute()),
                        ))
                        .child(ghost_button(
                            "copy-link",
                            "复制链接",
                            cx.listener(|this, _, _, cx| this.copy_link(cx)),
                        ))
                        .child(div().flex_1())
                        .child(ghost_button(
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
                        .child(ghost_button(
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
                );

        if self.show_settings {
            root = root.child(settings_card(&self.config, s.port, self.autostart, cx));
        }

        // ---- network info -------------------------------------------
        root = root.child(
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
                .child(div().text_sm().text_color(rgb(0x6B7280)).child(format!(
                    "端口: {}   ·   播放器: {}",
                    s.port,
                    self.core.engine.player.name()
                )))
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

        root.child(div().text_xs().text_color(rgb(0x9CA3AF)).child(format!(
            "状态: {}   ·   提示: 拖入本地文件可直接播放   ·   详细日志见控制台",
            s.transport_state.as_str(),
        )))
    }
}

/// Settings panel. Text fields are intentionally file-based for now: gpui
/// 0.2.2 has no input element (and no IME plumbing), so we expose the values
/// read-only plus a shortcut to the config file and a one-click backend switch.
fn settings_card(
    config: &Config,
    actual_port: u16,
    autostart: bool,
    cx: &mut Context<RootView>,
) -> Div {
    let player = match config.player {
        PlayerKind::Mpv => "mpv（外部进程）",
        PlayerKind::Null => "null（仅协议联调）",
    };
    let mpv_path = config
        .mpv_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "自动（PATH 中的 mpv）".to_string());

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
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("设置"),
                )
                .child(ghost_button(
                    "open-config",
                    "打开配置文件",
                    cx.listener(|this, _, _, cx| this.open_config(cx)),
                )),
        )
        .child(info_row("设备名称", config.friendly_name.clone()))
        .child(info_row(
            "端口",
            if config.port == actual_port {
                format!("{actual_port}")
            } else {
                format!("配置 {} / 实际 {actual_port}", config.port)
            },
        ))
        .child(info_row("播放后端", player.to_string()))
        .child(info_row("mpv 路径", mpv_path))
        .child(info_row(
            "开机自启",
            if autostart { "已开启" } else { "未开启" },
        ))
        .child(info_row("UDN", config.udn.clone()))
        .child(
            div()
                .text_xs()
                .text_color(rgb(0x9CA3AF))
                .child(format!("配置文件：{}", Config::config_path().display())),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(ghost_button(
                    "toggle-player",
                    match config.player {
                        PlayerKind::Mpv => "切换为 null 后端",
                        PlayerKind::Null => "切换为 mpv 后端",
                    },
                    cx.listener(|this, _, _, cx| this.toggle_player(cx)),
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x9CA3AF))
                        .child("改名 / 改端口 / 填 mpv 路径：编辑 config.json 后重启应用"),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(ghost_button(
                    "toggle-autostart",
                    if autostart {
                        "关闭开机自启"
                    } else {
                        "开启开机自启"
                    },
                    cx.listener(|this, _, _, cx| this.toggle_autostart(cx)),
                ))
                .child(div().text_xs().text_color(rgb(0x9CA3AF)).child(
                    "Windows：写入 HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                )),
        )
}

fn info_row(label: &'static str, value: impl Into<String>) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(80.0))
                .text_sm()
                .text_color(rgb(0x6B7280))
                .child(label),
        )
        .child(div().flex_1().text_sm().child(value.into()))
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

/// Secondary button: white background, grey border.
fn ghost_button(
    id: &'static str,
    label: impl IntoElement + 'static,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .px_3()
        .py_2()
        .rounded_md()
        .bg(rgb(0xFFFFFF))
        .border_1()
        .border_color(rgb(0xD1D5DB))
        .hover(|s| s.bg(rgb(0xF3F4F6)))
        .active(|s| s.bg(rgb(0xE5E7EB)))
        .text_color(rgb(0x374151))
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

/// Open a file with the OS default application.
fn open_in_default_app(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .arg("/C")
            .arg("start")
            .arg("")
            .arg(path)
            .spawn()
            .map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map(|_| ())
    }
}

// ---- start-at-login (Windows registry Run key) ---------------------------

#[cfg(target_os = "windows")]
const AUTOSTART_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(target_os = "windows")]
const AUTOSTART_VALUE: &str = "MacastGpui";

fn is_autostart_enabled() -> bool {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("reg")
            .args(["query", AUTOSTART_KEY, "/v", AUTOSTART_VALUE])
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

fn set_autostart(enable: bool) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        let exe = std::env::current_exe()?;
        let status = if enable {
            let quoted = format!("\"{}\"", exe.display());
            std::process::Command::new("reg")
                .args([
                    "add",
                    AUTOSTART_KEY,
                    "/v",
                    AUTOSTART_VALUE,
                    "/t",
                    "REG_SZ",
                    "/d",
                    quoted.as_str(),
                    "/f",
                ])
                .status()?
        } else {
            std::process::Command::new("reg")
                .args(["delete", AUTOSTART_KEY, "/v", AUTOSTART_VALUE, "/f"])
                .status()?
        };
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "reg.exe 返回 {}",
                status.code().unwrap_or(-1)
            )))
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = enable;
        Err(std::io::Error::other(
            "开机自启目前仅支持 Windows".to_string(),
        ))
    }
}
