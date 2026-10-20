# 实施方案：mpv 后端窗口内渲染（libmpv SW render）

> 状态：**已实施**（M1–M4 落地，见提交 `fad15f5`）
> 目标：让 mpv 后端的画面也画在 gpui 窗口内（与 alhazen 后端体验一致），不再弹外部播放器窗口。
> **落地后的实际形态（与本文早期设计不同）**：不新增后端（`PlayerKind` 仍是 `alhazen` / `mpv`），
> 也不再需要 `render_mode`——mpv 后端**固定**为进程内 libmpv + SW render；
> `PlayerKind::Mpv` 直接构造 `player::libmpv::LibmpvPlayer`，配置项是 `player.libmpv_path`。
> 下文 §3.2 提到的 `PlayerKind::Libmpv` / `render_mode` 属早期设计，已不采用。
> 相关：README「已知边界 / Roadmap」、`docs/plan-builtin-backend.md`（更早的一次内嵌尝试）。

## 1. 结论摘要

**可行**。路径是：mpv 以**进程内库**（libmpv）运行，用它的 **SW render API** 把画面渲染进我们提供的内存 buffer（BGRA），再由 gpui 的 `RenderImage` → `paint_image` 画进窗口——**与 alhazen 后端完全相同的 gpui 渲染入口**。

### 关于「是不是不需要 mpv-cli 了」

不是"不需要 mpv-cli"，而是**两条 mpv 路线的依赖不同**，二者会并存：

| 后端 | 形态 | 需要 | 画面位置 |
|:---|:---|:---|:---|
| `mpv`（现有） | 外部进程 + JSON IPC | `mpv.exe`（或 `mpv_path`） | mpv 自己的窗口 |
| `mpv` + `render_mode=embedded` | 进程内库 + SW render | `libmpv-2.dll`（或 `player.mpv.libmpv_path`） | **gpui 窗口内** |
| `alhazen`（现有，默认） | 进程内解码 | 无外部依赖 | gpui 窗口内 |

- **`embedded` 模式不需要 `mpv.exe`**：它是 mpv 的库版本，直接加载 `libmpv-2.dll` 在进程内跑
- 现有 `mpv` 后端**保留不动**：用户已装 mpv.exe 时它仍可用，且不需要 DLL
- `config.mpv_path` 语义不变（指向 mpv.exe）；新增 `libmpv_path` 指向 DLL，互不干扰

## 2. 已验证的关键事实

### 2.1 gpui 0.2.2 只有一个渲染入口

| 路线 | 结论（前一次尝试已实测） |
|:---|:---|
| gpui `surface` 元素 | Windows 上是空实现（`draw_surfaces` no-op），只支持 macOS `CVPixelBuffer` |
| 子 HWND 内嵌 | gpui 走 DirectComposition，窗口内容合成在子 HWND **之上**，视频被盖住 |
| 外部 GPU 纹理直交 | gpui 未暴露任何纹理共享接口，必须 fork 渲染器（Blade/wgpu）才行 |
| **`RenderImage` → `paint_image`** | **唯一可行路径**（alhazen 后端用的就是这个） |

因此本方案与 alhazen 一样：帧必须以 **CPU 侧 BGRA** 形式交给 gpui。

### 2.2 libmpv 的 SW render API 可用

高层 crate `libmpv2` 6.0.0 的 `RenderParamApiType` **只有 `OpenGl`**，不支持 SW；但底层 `libmpv2-sys` 4.0.1 的预生成绑定**完整暴露了 SW 所需常量**（`pregenerated_bindings.rs`）：

```
MPV_RENDER_API_TYPE_SW                    = b"sw\0"          // 第 104 行
mpv_render_param_type_MPV_RENDER_PARAM_SW_SIZE     = 17
mpv_render_param_type_MPV_RENDER_PARAM_SW_FORMAT   = 18
mpv_render_param_type_MPV_RENDER_PARAM_SW_STRIDE   = 19
mpv_render_param_type_MPV_RENDER_PARAM_SW_POINTER  = 20
```

结论：**用 `libmpv2-sys` 直接写 FFI（约 40 行）**，绕过 `libmpv2` 高层 crate 的 OpenGL 限制。
（备用方案：OpenGL render 到 FBO 再 `glReadPixels` 回读——需要自建 GL context，更绕，不采用。）

### 2.3 DLL 体积与获取

- `libmpv-2.dll`：**121 MB**（来自 shinchiro/mpv-winbuild-cmake 的 `mpv-dev-x86_64-*.7z`）
- 本机已提取到 `.workbuddy/mpv-dev/libmpv-2.dll`（不入库，仅本地验证用）
- **分发策略（已定）**：打包**不含** DLL，由 `config.libmpv_path` 指定，找不到则该后端不可用并给出明确提示

## 3. 架构设计

### 3.1 总体思路

沿用已有的 `VideoHost` 边界（core 不碰 Win32 / gpui，app 负责窗口侧）：

```
core                                    app (gpui)
────────────────────────────────        ────────────────────────────────
player/libmpv.rs                        MpvVideoHost（实现 VideoHost）
  LibmpvPlayer: Player trait              ├─ 加载 libmpv-2.dll
  ├─ play/pause/seek/volume/mute          ├─ mpv_handle + mpv_render_context
  │  → VideoHost.post(VideoCommand) ─────▶├─ 每帧 mpv_render_context_render()
  └─ 事件 ← EventSink ◀──────────────────┤  → 最新帧存入 Arc<[u8]>
                                          └─ frame() 供 UI 查询
                                                 │
                                          MpvView（gpui Element）
                                            paint: frame() → RenderImage
                                                   → paint_image → 窗口内
```

### 3.2 需要的接口变更

**core `player/video_host.rs`**：`VideoHost` trait 增加帧查询与尺寸设置

```rust
pub trait VideoHost: Send + Sync {
    fn post(&self, cmd: VideoCommand);
    fn set_event_sink(&self, sink: EventSink);
    fn is_fullscreen(&self) -> bool;
    /// 当前帧（BGRA，宽高 + 数据），`None` 表示还没有可显示的画面。
    fn frame(&self) -> Option<(u32, u32, Arc<[u8]>)>;   // 新增
    /// 目标渲染尺寸（设备像素），用于让 mpv 按显示大小缩放，避免 4K 全量转换。
    fn set_render_size(&self, w: u32, h: u32);           // 新增
}
```

（`NullVideoHost` 用于 alhazen，`frame()` 返回 `None`、`set_render_size` 空实现，不受影响。）

**core `config.rs`（已实现）**：

```rust
pub enum PlayerKind { Alhazen, Mpv }            // 不变，#[default] Alhazen
pub enum MpvRenderMode { Auto, External, Embedded }

pub struct PlayerConfig {
    pub kind: PlayerKind,
    pub fullscreen_on_cast: bool,
    pub render_mode: MpvRenderMode,   // 新增
    pub mpv: MpvConfig { exe_path: Option<PathBuf>, libmpv_path: Option<PathBuf> },
}
```

`build_player(&PlayerConfig, video_host)` 按 `render_mode` 选择实现；`Embedded` 未接线前
和 `Auto`/`External` 一样走外部 mpv 进程。

**core `player/mod.rs`**：`build_player` 增加 `PlayerKind::Libmpv => libmpv::LibmpvPlayer::new(host)`；
host 缺失或 DLL 加载失败时返回错误（**不回退 null**——按用户要求已取消回退逻辑，设置面板明确提示）。

**app 侧新增 `crates/macast-app/src/mpv_view.rs`**：
- `MpvVideoHost`：持有 libmpv 实例、`Arc<Mutex<Option<(u32,u32,Arc<[u8]>)>>>` 帧槽、命令处理
- `mpv_view(host) -> MpvView`：gpui Element，`paint` 时取帧 → `RenderImage` → `paint_image` → 旧帧 `drop_image`（需 `Retired` 机制，参考 alhazen-gpui 的 `RETIRE_AFTER = 3`）→ `request_animation_frame`

### 3.3 渲染循环（SW render 调用序列）

```c
// 初始化
mpv_set_option_string(mpv, "vo", "libmpv");        // 关键：画面交给 render API
mpv_set_option_string(mpv, "keep-open", "always");
mpv_initialize(mpv);

mpv_render_param create[] = {
    { MPV_RENDER_PARAM_API_TYPE, (void*)"sw" }, { 0, NULL }
};
mpv_render_context_create(&ctx, mpv, create);

// 每帧（在 UI 线程的 paint 里，或独立渲染线程）
int  size[2] = { w, h };
int  stride  = w * 4;
mpv_render_param rp[] = {
    { MPV_RENDER_PARAM_SW_SIZE,    size },
    { MPV_RENDER_PARAM_SW_FORMAT,  (void*)"bgra" },
    { MPV_RENDER_PARAM_SW_STRIDE,  &stride },
    { MPV_RENDER_PARAM_SW_POINTER, buffer },
    { 0, NULL }
};
if (mpv_render_context_render(ctx, rp) == 0)
    mpv_render_context_report_swap(ctx);
```

驱动方式：`mpv_render_context_set_update_callback` 收到更新后在 gpui 侧 `cx.notify()`；
paint 前后调用 `mpv_render_context_update(ctx)` 判断是否 `MPV_RENDER_UPDATE_FRAME`（有帧才重绘，无帧不空转）。

### 3.4 DLL 探测顺序（Windows）

1. `player.mpv.libmpv_path`（若配置且文件存在）
2. 环境变量 `MACAST_LIBMPV`
3. exe 同目录 `libmpv-2.dll`
4. `PATH` 中的 `libmpv-2.dll` / `mpv-2.dll`
5. 常见安装位置：`%LOCALAPPDATA%\Programs\Scoop\apps\libmpv\current\libmpv-2.dll`、
   `%LOCALAPPDATA%\Programs\Scoop\apps\mpv\current\libmpv-2.dll`

全部找不到 → 回落到外部 mpv 进程（`Auto`），或在 `Embedded` 下明确报错"未找到 libmpv-2.dll，请在配置中指定 player.mpv.libmpv_path"。

## 4. 实施里程碑

每步独立可验证。

| 里程碑 | 内容 | 验证方式 |
|:---|:---|:---|
| **M0** 配置与接线 | ✅ 已完成（2026-10-20）：配置改为 `device`/`player`/`ui`/`network` 分组，新增 `MpvRenderMode` 与 `mpv.exe_path`/`mpv.libmpv_path`，设置面板显示 mpv 程序与渲染方式 | `cargo test` 23 项通过；删除旧配置后启动生成新结构 |
| **M1** libmpv 取帧（脱离 UI） | `libmpv2-sys` FFI 封装：加载 DLL、创建 handle、SW render 到 buffer | 写一个小 demo/测试：投一个本地视频，把收到的帧存成 PNG，肉眼确认画面正确 |
| **M2** core 侧 Player | `player/libmpv.rs`：命令转发 + 事件回传（`Loaded/Position/Duration/Playing/Paused/Stopped/Ended/Volume/Mute`） | SOAP 投屏，`GetTransportInfo` / `GetPositionInfo` 状态与进度正确 |
| **M3** gpui 渲染 | `mpv_view.rs` Element + RootView 在 libmpv 后端时用它替代 artwork | 实机：画面出现在 hero 卡内，无外部窗口 |
| **M4** 生命周期与全屏 | Stop/Ended 收起画面；每次投屏全屏（复用 `cast_seq` 逻辑）；Esc 退出全屏（复用 `GetAsyncKeyState` 轮询） | 与 alhazen 同样的实测矩阵 |
| **M5** 性能与文档 | 帧率/耗时统计（参考 `ALHAZEN_DEBUG`）、README/GUIDE 更新、打包说明注明不含 DLL | 1080p60 实测帧率；`cargo fmt/clippy/test` 全绿 |

## 5. 性能预算

与 alhazen 共用 gpui 的整帧上传通道，因此上限相近：

| 环节 | 预估（1080p60，每帧） |
|:---|:---|
| mpv 解码（可走 `--hwdec=auto` 后仍需回读） | 1–3 ms |
| mpv SW render（swscale YUV→BGRA，CPU） | 3–8 ms ⚠️ 主要新增开销 |
| `bgra.to_vec()` + `RenderImage` | 1–2 ms |
| gpui `paint_image`（纹理上传） | 2–4 ms |
| **合计** | **7–17 ms/帧** → 预期 **50–60 fps** |

- **1080p60**：预期 50–60fps（待实测，若不足可把 `set_render_size` 限制到显示尺寸以省掉缩放与上传）
- **4K60**：不乐观（与 alhazen 同一道上传瓶颈），列为已知限制
- 优化手段：`vo=libmpv` + `hwdec=auto`（解码用 GPU、仅回读转换在 CPU）、按显示尺寸渲染

## 6. 风险与缓解

| 风险 | 缓解 |
|:---|:---|
| 121 MB DLL 分发 | **已定：不打包**，由 `player.mpv.libmpv_path` 指定；找不到时明确提示而非静默失败 |
| mpv 版本 API 差异 | 加载后校验必需符号（`mpv_render_context_create/render/report_swap`），缺失则报错 |
| SW render 是软件色彩转换，CPU 占用高 | 限制渲染尺寸到显示大小；必要时允许 `--gpu-context` 之外的 hwdec 回读路径 |
| 与现有 mpv 后端并存导致混淆 | 设置面板分别说明："mpv（外部窗口，需 mpv.exe）" / "libmpv（窗口内，需 libmpv-2.dll）" |
| sprite atlas 每帧上传（gpui 限制） | 与 alhazen 同限；旧帧 `drop_image` 需延迟数帧（参考 `RETIRE_AFTER`） |
| serde 兼容 | `PlayerKind` 加变体是向后兼容的（旧配置 `"player":"mpv"` 仍有效） |

## 7. 待确认

- 是否允许 embedded 模式使用 GPU 解码（`hwdec=auto`）后 **GPU→CPU 回读**？理论上更快，但 mpv 的 hwdec + SW render 组合需要实测是否稳定
- 字幕 / 音轨支持：mpv 引擎本身支持，MVP 阶段不接 UI 控件
- 是否需要把 libmpv 也用于纯音频（无视频时显示 artwork，复用 alhazen 的 `has_video` 判定思路）
