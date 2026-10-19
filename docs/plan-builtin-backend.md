# 实施计划：内置播放后端（Windows Media Foundation）

> 状态：**待实施** ｜ 拟定：2026-10-19 ｜ 关联：GUIDE §9「视频嵌入 gpui 窗口」
>
> 决策记录（已确认）：
>
> | 决策点 | 结论 |
> |---|---|
> | 解码方案 | Media Foundation（mfplay `IMFPMediaPlayer` API，直渲 HWND，系统零依赖） |
> | 显示位置 | 主窗口内嵌：视频子 HWND 覆盖 hero 卡片区域，控制条留在窗口内 |
> | 投屏全屏 | 每次 cast 开始整个应用窗口进入 gpui 全屏，Esc 退出（复用 `mpv_fullscreen` 配置） |
> | 字幕 | MVP 不做（trait 方法 no-op，`DisplayCurrentSubtitle` 状态保留） |

## 目标

新增第三个播放后端 `PlayerKind::Builtin`：使用 Windows 系统内置解码器（Media Foundation），视频**内嵌渲染在主窗口**，不再依赖外部 mpv 窗口。mpv / null 后端保留不动。

## 关键技术事实（已探索验证）

- gpui 0.2.2 `surface` 元素 Windows 上是空壳（仅 macOS `CVPixelBuffer`，Windows 渲染器 `draw_surfaces` 直接丢弃）→ GPU 纹理共享不可行，**子 HWND 是唯一现实路径**
- gpui 窗口是普通顶层 HWND（`WS_EX_NOREDIRECTIONBITMAP` 不影响子窗口合成）；`events.rs` 只处理 WM_SIZE/WM_PAINT/WM_DPICHANGED 等，不会干扰子窗口
- HWND 获取模式现成：`window_handle_of()`（`views/mod.rs:614`，raw_window-handle 0.6）
- mfplay 官方保证回调在创建线程**串行、不重入**；创建线程必须有消息泵 → 全部 MF 操作收敛在 gpui 主线程（自带消息泵）
- mfplay **不推送周期性 position 事件** → 主线程 `SetTimer` 1s 轮询 `GetPosition`/`GetDuration`
- windows crate 0.61.3 已是依赖；需新增 features：`Win32_Media_MediaFoundation`、`Win32_System_Com`、`implement`（COM 回调 `#[implement(IMFPMediaPlayerCallback)]`）
- `build_player`（`player/mod.rs:62`）与 `MacastCore::start`（`core.rs:99`）各只有一个调用点（`core.rs:100` / `main.rs:83`）

## 架构

```
tokio workers (DLNA/bridge)                main thread (gpui 消息泵)
────────────────────────────              ─────────────────────────────────
Player trait 调用                           message-only window wndproc
  avtransport.rs → BuiltinPlayer            ├─ 子 HWND 创建/Move/Show/Hide（物理 px）
  (core, 只做命令转发)   │ post(VideoCommand) ├─ MFPlay: CreateMediaItemFromURL/SetMediaItem/
                        ▼                    │   Play/Pause/Stop/SetPosition/SetVolume/...
              PostMessageW(WM_APP) ───────►  ├─ WM_TIMER(1s): GetPosition → Position 事件
                                             └─ OnMediaPlayerEvent（串行回调）
  bridge_loop ◄── broadcast::Sender(PlayerEvent) ◄── EventSink
```

- core 侧 `BuiltinPlayer` 零 Win32 依赖，只转发命令 + 发事件；app 侧 `MfVideoHost` 实现 `VideoHost` trait
- `VideoCommand::Play { url, start_secs, title, fullscreen }` 携带全屏标志 → 复刻 mpv「每次投屏重新应用全屏」语义（`mpv.rs` `media_commands()` 先例）

## 文件变更清单

### macast-core

| 文件 | 变更 |
|---|---|
| `src/player/video_host.rs`（新增） | `VideoHost` trait（`post(VideoCommand)`、`set_event_sink(EventSink)`）、`VideoCommand` 枚举（EnsureCreated/Play/Resume/Pause/Stop/Seek/SetVolume/SetMute/SetSpeed/MoveSurface/ShowSurface/HideSurface/Shutdown）、`EventSink = Arc<dyn Fn(PlayerEvent)+Send+Sync>` |
| `src/player/builtin.rs`（新增，cfg(windows)） | `BuiltinPlayer`：trait 方法全部转为 `host.post(...)`；`set_volume/set_mute` 同时直接发 Volume/Mute 事件；`add_subtitle/set_subtitle_visible/show_text` no-op；`name() = "builtin"`。可测纯函数：`secs_to_100ns`/`ns100_to_secs`（钳 0/溢出）、`clamp_rate`（0.0625..16）、`map_mfp_event(i32, detail) -> Option<PlayerEvent>`（13 个 MFP_EVENT_TYPE 全分支，windows 类型挡在轻量自有 enum 外） |
| `src/player/mod.rs` | `build_player` 加第 4 参 `host: Option<Arc<dyn VideoHost>>`；match 加 `PlayerKind::Builtin` 分支（host 缺失/初始化失败 → NullPlayer + warn，复用 mpv 回退模式） |
| `src/config.rs` | `PlayerKind` 加 `Builtin`（serde snake_case → `"builtin"`）；旧配置无此字段不受影响；旧程序读新配置判 corrupt 回退默认——单向升级，可接受 |
| `src/core.rs` | `MacastCore::start(config, video_host: Option<Arc<dyn VideoHost>>)` 传给 build_player |

### macast-app

| 文件 | 变更 |
|---|---|
| `src/video_host.rs`（新增，cfg(windows)） | `MfVideoHost`：主线程注册窗口类 + `CreateWindowExW(HWND_MESSAGE)` message-only window；`post` = 命令装箱指针 → `PostMessageW(WM_APP)`；wndproc（GWLP_USERDATA 存状态）：`parent/child: Option<HWND>`（WS_CHILD\|WS_CLIPSIBLINGS 黑底）、`mf: Option<IMFPMediaPlayer>`、`#[implement(IMFPMediaPlayerCallback)]` 回调、`sink`、`last_rect`；首条命令 `CoInitializeEx(APARTMENTTHREADED)`（容忍 S_FALSE/RPC_E_CHANGED_MODE）；`attach_parent(gpui_hwnd)`；`Shutdown` → `IMFPMediaPlayer::Shutdown` + `DestroyWindow(child)` |
| `src/main.rs` | `start()` 前创建 `MfVideoHost`，`start(Config::load(), Some(host.clone()))`；`open_window` 后 `host.attach_parent(hwnd)`（复用 window_handle_of 模式）；host 传入 RootView |
| `src/views/mod.rs` | ① RootView 加 `video_bounds: Rc<Cell<Bounds<Pixels>>>` + host 字段；② `hero_card` 最外层叠 canvas 采集 bounds（模式同 seek_bounds）；③ render 尾部：`runtime_backend=="builtin"` 时 bounds×`scale_factor` 变化 → `post(MoveSurface)`；有媒体 → ShowSurface，无媒体/停止 → HideSurface；④ `segmented_player` 加第三选项「内置」；⑤ `config.player==Builtin` 时隐藏「mpv 路径」行，保留「投屏时全屏」（同样生效）；fallback chip 扩展 `Builtin && runtime=="null"` → 「内置播放初始化失败」 |
| `Cargo.toml` | windows features 增：`Win32_Media_MediaFoundation`、`Win32_System_Com`、`implement`、`Win32_Graphics_Gdi` |

### 全屏（关键细节）

gpui 全屏后 hero 卡受 `max_w 1100 / max_h 440` 限制**不会铺满客户区**，因此：

- RootView 加 `video_fullscreen: bool` 状态；cast 开始（`Play{fullscreen:true}`）→ 若 `!window.is_fullscreen()` 调 `window.toggle_fullscreen()`（gpui 0.2.2 window.rs:4122 有完整 Windows 实现）并置位
- `video_fullscreen` 时：跳过 hero bounds，直接 `post(MoveSurface{ 0,0, viewport_size×scale })`（视频铺满客户区=铺满显示器），渲染改为纯黑背景（视频盖在上面）
- Esc 退出：gpui action（`KeyBinding::new("escape", …)`）+ root div `.key_context("RootView")` + `.track_focus()`；action 中复位 `video_fullscreen` 并 `toggle_fullscreen()`。**风险**：焦点需在 gpui 窗口（子 HWND 不取焦点，必要时 wndproc 加 `WM_MOUSEACTIVATE → MA_NOACTIVATE`）

## 里程碑（每步独立可验证）

| # | 内容 | 验证 |
|---|---|---|
| M0 | PlayerKind::Builtin + segmented 第三选项 + build_player/start 加 host 参数（Builtin 暂回退 Null）+ windows features | cargo test；UI 选「内置」→ footer 显示 null 后端，config 写入 `"player":"builtin"` 重启保留 |
| M1 | video_host trait + MfVideoHost 骨架（message window、attach_parent、命令日志、EventSink）+ main.rs 接线 | 日志按序出现命令；运行无异常 |
| M2 | 子 HWND + 几何：hero bounds 采集、MoveSurface 物理换算、Show/Hide | 黑色子窗口精确覆盖 hero 卡；resize/最大化跟随 |
| M3 | MFPlay 播放管线：Play（本地文件 + `http://127.0.0.1:5069/...` 均测）/Pause/Stop/Seek/Volume/Mute/WM_TIMER position/事件映射 | 出画出声；进度条走；UI 控制全部生效；Ended → NO_MEDIA_PRESENT |
| M4 | 显隐完善：`HasVideo()` 判定（纯音频保留 artwork）、Stop/Ended/Error 隐藏 | 手机投纯音乐 → artwork 不被黑窗盖住 |
| M5 | 全屏：cast start → toggle_fullscreen + video_fullscreen 布局切换 + Esc 退出 + 每次 cast 重新应用 | mpv_fullscreen 开/关各测；连续两次投屏都全屏 |
| M6（可选） | axum 静态 handler 加 Range/206（HTTP seek 目前不可用，MVP 标注限制） | curl -H "Range: bytes=0-1023" 验证 206 |

## 测试

- 单测（纯函数）：100ns 换算（0/负/溢出）、clamp_rate、`map_mfp_event` 全分支、config `"builtin"` round-trip + legacy 配置回归；mpv 既有测试不动
- 手工 QA：手机投视频/纯音频/图片；播放中二次投屏（replace）；Stop/Ended 后 artwork 恢复；Esc 全屏；DPI 切换后子窗口跟随；播放中 quit（无崩溃无孤儿窗口）
- 收尾按仓库惯例：cargo fmt/test/clippy → release 构建 → 签名提交 → push → CI → 更新 README/GUIDE/截图

## 风险与缓解

| 风险 | 缓解 |
|---|---|
| mfplay 标注 legacy | Win10/11 全量携带；VideoHost trait 隔离，未来可整体换 IMFMediaEngine 实现不动 DLNA/UI |
| 编解码缺失（HEVC 扩展） | ERROR 事件 → UI 显示 ERROR_OCCURRED + 提示安装「HEVC 视频扩展」 |
| gpui 重绘与子窗口闪烁 | WS_CLIPSIBLINGS；先 Move 再 Show；MVP 接受偶发一帧 |
| Esc 焦点问题 | track_focus + MA_NOACTIVATE 兜底 |
| mfplay 回调线程 | 官方保证主线程串行不重入；MF 调用全部收敛主线程 |
| HTTP seek | 本机 HTTP 服务无 Range 支持，MVP 标注限制（M6 可选） |

## 本期不做

- 字幕（`add_subtitle`/`set_subtitle_visible` no-op，`DisplayCurrentSubtitle` 状态保留）
- libmpv 内嵌后端（VideoHost trait 已预留扩展点）
- axum Range/206（M6 可选）
