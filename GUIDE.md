# 用 gpui 复刻 Macast — 实施指南

> 目标：用 **Rust + gpui** 复刻 [Macast](https://github.com/xfangfang/Macast)（一个用 mpv 做后端的 **DLNA 媒体接收端 / 投屏接收器**）。
> 本目录已经为你准备好了**可编译的项目骨架**（`crates/macast-core` 协议核心 + `crates/macast-app` gpui 界面），本指南讲解原理、拆解步骤、给出测试方法。

---

## 目录

1. [原版 Macast 在做什么](#1-原版-macast-在做什么)
2. [复刻方案与技术选型](#2-复刻方案与技术选型)
3. [DLNA/UPnP 协议速查（必读）](#3-dlnaupnp-协议速查必读)
4. [项目骨架导览](#4-项目骨架导览)
5. [分阶段实施路线](#5-分阶段实施路线)
6. [测试指南](#6-测试指南)
7. [打包与分发](#7-打包与分发)
8. [常见坑与排查表](#8-常见坑与排查表)
9. [进阶方向](#9-进阶方向)
10. [参考资料](#10-参考资料)

---

## 1. 原版 Macast 在做什么

Macast 的本质：**让你的电脑在局域网里伪装成一台"电视/DLNA 渲染设备"**，手机视频 App 的"投屏到电视"按钮里就会出现它，点一下就把视频/图片/音乐推过来播放。

### 1.1 原版架构（Python）

```
手机 (DLNA Control Point)
   │  ① SSDP 广播 M-SEARCH "哪里有 MediaRenderer？"
   ▼
┌─────────────────────────── Macast (Python) ┐
│  ssdp.py        SSDP 服务器：应答 M-SEARCH、周期性 NOTIFY alive   │
│  server.py      cherrypy HTTP 服务器（含自动选端口）              │
│  protocol.py    UPnP 核心：状态变量表 + SOAP 动作分发 + GENA 事件  │
│  renderer.py    Renderer 抽象基类（set_media_* 一组方法）         │
│  macast_renderer/mpv.py   spawn mpv 进程，用 JSON IPC 控制它       │
│  macast.py      pywebview + Vue 的设置窗口；pystray 托盘图标       │
└──────────────────────────────────────────────────────────────────┘
   │  ② GET /description.xml（拿设备描述）
   │  ③ POST /AVTransport/action（SOAP：SetAVTransportURI + Play）
   ▼
mpv 独立窗口播放推流过来的 URL
   │  ④ mpv IPC 事件（time-pos/duration/pause…）回传
   ▼
protocol.set_state_*() → GENA NOTIFY 推给订阅了的手机
```

### 1.2 关键行为细节（读源码整理，复刻时照抄即可）

| 行为 | Macast 的做法 | 本骨架的做法 |
|---|---|---|
| HTTP 端口 | `DEFAULT_PORT = 0`，OS 随机分配，写回配置 | 默认 5069，被占用则退到随机端口 |
| UDN（设备 UUID） | 首次运行生成，持久化 | 首次运行生成，存 `config.json` |
| SSDP 组播 | 每个网卡一个 socket，`IP_MULTICAST_IF` 绑定出口 | 同左（`ssdp.rs`） |
| M-SEARCH 应答 | 随机延迟 `0..MX` 秒，从**与请求方同子网**的网卡发出 | 同左 |
| NOTIFY alive | 每 3 秒一次，`max-age=66` | 默认 30 秒一次，`max-age=1800`（可配置） |
| 设备描述 URL | `GET /description.xml`；控制 `/AVTransport/action`；事件 `/AVTransport/event`；SCPD `/dlna/AVTransport.xml` | 完全相同 |
| SOAP 分发 | 按命名空间取服务名，按标签名取动作名，查表分发 | 同左（`soap.rs` + `services/`） |
| mpv 启动 | `--input-ipc-server=<pipe/sock> --idle=yes --hwdec=yes …` | 同左 + `--force-window=immediate` |
| mpv 控制 | JSON IPC：`loadfile url replace`、`set_property pause/volume/mute/speed`、`seek N absolute`、`sub-add`、`show-text` | 同左 |
| 状态回传 | `observe_property` 订阅 time-pos/duration/pause/volume/mute/speed → 改状态变量 → GENA 推 LastChange | 同左 |
| GENA 事件 | SUBSCRIBE 返回 SID；立即推一次全量状态；之后推 LastChange 包裹的增量 | 同左 |
| 界面 | pywebview + Vue；托盘用 pystray | gpui 原生窗口（托盘见 §9） |

---

## 2. 复刻方案与技术选型

### 2.1 架构图

```
                    ┌──────────────────────────────────────────┐
                    │            macast-app (gpui)             │
                    │  RootView: 状态卡片/进度条/播放/音量/复制   │
                    └───────────────┬──────────────────────────┘
                                    │ watch::Receiver<PlaybackSnapshot>
                                    │ （UI 只读快照，控制调用走 MacastCore）
┌───────────────────────────────────▼──────────────────────────┐
│                     macast-core (无 UI)                        │
│                                                            │
│  ssdp.rs ──────► 设备上线/被发现（M-SEARCH 应答、NOTIFY）        │
│  http.rs ──────► axum 路由：                                    │
│       GET  /description.xml        （设备描述）                 │
│       GET  /dlna/{Service}.xml     （SCPD 服务描述）            │
│       POST /{Service}/action       （SOAP 控制）                │
│       SUBSCRIBE/UNSUBSCRIBE /{Service}/event （GENA 事件订阅）  │
│  services/ ────► AVTransport / RenderingControl / ConnectionManager
│  state.rs ─────► UPnP 状态变量表（RwLock<HashMap>）+ UI 快照通道 │
│  eventing.rs ──► GENA 订阅注册表 + LastChange NOTIFY 推送        │
│  player/ ──────► Player trait（对应 Macast 的 Renderer 基类）    │
│       alhazen.rs ─► 默认后端：转发给 UI 的 alhazen-gpui 播放器   │
│       libmpv.rs  ─► 进程内 libmpv + SW render → BGRA 帧         │
└──────────────────────────────────────────────────────────────┘
```

**图片投屏（`MediaKind`）**：`SetAVTransportURI` 会用 DIDL `upnp:class` / `res@protocolInfo` /
URI 扩展名判定媒体类型（`state::MediaKind`）。判为 `Image` 时 core **不调用 player**（只 `stop()` 掉上一个），
`Play/Pause/Seek` 也不转给 player；UI 侧 `MacastCore::fetch_bytes(uri)`（http/file）取字节 → `image` 解码
（RGBA→BGRA）→ `paint_image` 画进 hero 卡片 / 全屏。切到 A/V 或被 Stop 时 `media_kind` 复位，画面清除。

### 2.2 选型理由

| 需求 | 选择 | 理由 |
|---|---|---|
| UI 框架 | **gpui 0.2.2**（crates.io） | Zed 的 GPU UI 框架，2026-04 已随 Zed 1.0 成熟；Windows 无需额外 feature（Win32 + wgpu/D3D12 + cosmic-text） |
| 异步运行时 | tokio（full） | SSDP/HTTP/IPC 全异步；gpui 主线程只负责渲染，通过 `watch` 通道收快照 |
| HTTP 框架 | axum 0.8 | 路由/提取器省心；任意方法（SUBSCRIBE）用 `any()` + 手判 |
| XML | roxmltree（解析）+ `format!`（生成） | 控制报文结构简单，树形 API 比事件 API 好写 |
| 播放后端 | **alhazen（默认，进程内解码）/ 进程内 libmpv（可选）** | 两种都画在 gpui 窗口内；alhazen 零外部依赖，mpv 只需 `libmpv-2.dll`（不需要 `mpv.exe`） |
| 组播 | socket2 | 需要 `SO_REUSEADDR`、`IP_ADD_MEMBERSHIP`、`IP_MULTICAST_IF` 这些 tokio 不直接暴露的能力 |
| 剪贴板 | arboard | gpui 0.2 的剪贴板 API 在 View 层不好够到，用独立小 crate 最稳 |

> **最终方案（已实施）**：视频内嵌 gpui 窗口走 **`RenderImage` → `paint_image`**（`surface` 元素 Windows 空壳、子 HWND 被 DirectComposition 盖住、无 GPU 纹理交接接口）。默认 alhazen 后端（纯进程内解码）用它；mpv 后端也用 libmpv 的 SW render 出 BGRA 帧走同一条路，**无需 `mpv.exe`，但要 `libmpv-2.dll`**。

---

## 3. DLNA/UPnP 协议速查（必读）

复刻工作量 80% 在协议。抓住这四层就够了：

### 3.1 SSDP（发现层，UDP 组播 239.255.255.250:1900）

**手机发起的搜索：**
```
M-SEARCH * HTTP/1.1
HOST: 239.255.255.250:1900
MAN: "ssdp:discover"
MX: 3
ST: urn:schemas-upnp-org:device:MediaRenderer:1
```

**我们的应答（单播发回请求方，延迟 0..MX 秒随机）：**
```
HTTP/1.1 200 OK
CACHE-CONTROL: max-age=1800
DATE: <RFC1123 时间>
EXT:
LOCATION: http://192.168.1.10:5069/description.xml
SERVER: Linux/5.0 UPnP/1.1 Macast-Gpui/0.1
ST: urn:schemas-upnp-org:device:MediaRenderer:1
USN: uuid:xxxx::urn:schemas-upnp-org:device:MediaRenderer:1
```

**主动通告（上线时 + 周期性，组播）：**
```
NOTIFY * HTTP/1.1
HOST: 239.255.255.250:1900
CACHE-CONTROL: max-age=1800
LOCATION: http://192.168.1.10:5069/description.xml
NT: urn:schemas-upnp-org:device:MediaRenderer:1
NTS: ssdp:alive
SERVER: ...
USN: uuid:xxxx::urn:schemas-upnp-org:device:MediaRenderer:1
```
下线时把 `NTS` 换成 `ssdp:byebye` 再发一遍。

**必须注册的 6 个 USN/ST 对**（与 Macast 一致）：
```
uuid:<udn>::upnp:rootdevice                                ST: upnp:rootdevice
uuid:<udn>                                                 ST: uuid:<udn>
uuid:<udn>::urn:schemas-upnp-org:device:MediaRenderer:1    ST: 同名
uuid:<udn>::urn:schemas-upnp-org:service:AVTransport:1     ST: 同名
uuid:<udn>::urn:schemas-upnp-org:service:RenderingControl:1 ST: 同名
uuid:<udn>::urn:schemas-upnp-org:service:ConnectionManager:1 ST: 同名
```
`ST: ssdp:all` 的搜索要对上面全部逐条应答。

### 3.2 设备描述（`GET /description.xml`）

手机拿到 LOCATION 后 GET 这个文档，知道有三个服务、控制/事件 URL 是什么。**骨架里的 `device.rs` 已生成**，要点：
- `<deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType>`
- 三个服务：AVTransport / RenderingControl / ConnectionManager
- `<dlna:X_DLNADOC>DMR-1.50</dlna:X_DLNADOC>`（声明 DLNA 渲染器档位）
- URL 全部用**相对路径**，这样换 IP/端口不用重新生成

### 3.3 SCPD（服务描述，`GET /dlna/AVTransport.xml` 等）

声明每个服务有哪些 action、参数、状态变量。**直接从 Macast（MIT）拷贝并内嵌**（`assets/`，见 NOTICE.txt），因为手写 20KB XML 易错且没必要。我们的 SOAP 响应不依赖它，只是客户端要 fetch。

### 3.4 SOAP 控制（`POST /AVTransport/action`）

**手机发来的投屏请求：**
```xml
<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"
            s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body>
    <u:SetAVTransportURI xmlns:u="urn:schemas-upnp-org:service:AVTransport:1">
      <InstanceID>0</InstanceID>
      <CurrentURI>http://192.168.1.5:8080/video.mp4</CurrentURI>
      <CurrentURIMetaData>&lt;DIDL-Lite ...&gt;...&lt;/DIDL-Lite&gt;</CurrentURIMetaData>
    </u:SetAVTransportURI>
  </s:Body>
</s:Envelope>
```
请求头带 `SOAPACTION: "urn:schemas-upnp-org:service:AVTransport:1#SetAVTransportURI"`。

**我们的成功响应**（`services/avtransport.rs` 里逐动作实现）：
```xml
<s:Envelope ...><s:Body>
  <u:SetAVTransportURIResponse xmlns:u="urn:schemas-upnp-org:service:AVTransport:1"/>
</s:Body></s:Envelope>
```

**错误响应**（HTTP 500 + UPnP 错误码）：
```xml
<s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring>
<detail><UPnPError xmlns="urn:schemas-upnp-org:control-1-0">
<errorCode>401</errorCode><errorDescription>Invalid Action</errorDescription>
</UPnPError></detail></s:Fault>
```

**必须实现的动作清单：**

| 服务 | 动作 | 说明 |
|---|---|---|
| AVTransport | `SetAVTransportURI` | 收 URL + DIDL 元数据（里面 `dc:title` 是标题），loadfile |
| AVTransport | `SetNextAVTransportURI` | 存起来即可 |
| AVTransport | `Play` / `Pause` / `Stop` / `Seek` | 转发给播放器 |
| AVTransport | `GetTransportInfo` | 返回 `CurrentTransportState/CurrentTransportStatus/CurrentSpeed` |
| AVTransport | `GetPositionInfo` | 返回进度、时长、URI |
| AVTransport | `GetMediaInfo` / `GetDeviceCapabilities` / `GetTransportSettings` / `GetCurrentTransportActions` | 查询类 |
| RenderingControl | `GetVolume` / `SetVolume` / `GetMute` / `SetMute` | 音量 |
| ConnectionManager | `GetProtocolInfo` | **返回 Sink 列表**（assets/SinkProtocolInfo.csv），决定手机认不认你的解码能力 |
| ConnectionManager | `GetCurrentConnectionIDs` / `GetCurrentConnectionInfo` | 查询类 |

`TransportState` 四个取值：`STOPPED / PLAYING / PAUSED_PLAYBACK / NO_MEDIA_PRESENT`。

### 3.5 GENA 事件（`SUBSCRIBE /AVTransport/event`）

- 客户端发 `SUBSCRIBE` + `CALLBACK: <http://手机IP:端口/回调>` → 我们回 `SID: uuid:...` + `TIMEOUT: Second-1800`，并**立即推一次全量状态**
- 之后状态变量变化时，向回调 URL 发 `NOTIFY` 请求：
```xml
<e:propertyset xmlns:e="urn:schemas-upnp-org:event-1-0">
  <e:property><LastChange>&lt;Event xmlns="urn:schemas-upnp-org:metadata-1-0/AVT/"&gt;
    &lt;InstanceID val="0"&gt;&lt;TransportState val="PLAYING"/&gt;
    &lt;RelativeTimePosition val="0:01:23"/&gt;&lt;/InstanceID&gt;&lt;/Event&gt;
  </LastChange></e:property>
</e:propertyset>
```
- 到期前客户端会发带 `SID` 的 SUBSCRIBE 续期；`UNSUBSCRIBE` 带 `SID` 注销

### 3.6 mpv JSON IPC（播放后端）

启动参数：
```
mpv --input-ipc-server=\\.\pipe\macast_gpui_mpv_12345   # Windows 命名管道
    --idle=yes --no-terminal --force-window=immediate
    --image-display-duration=inf --hwdec=auto
    --save-position-on-quit=yes --script-opts=osc-...
```
协议是**一行一条 JSON**：
```json
→ {"command": ["loadfile", "http://.../video.mp4", "replace"]}
→ {"command": ["observe_property", 1, "time-pos"]}
← {"event":"property-change","name":"time-pos","data":83.5,"id":1}
← {"event":"end-file","reason":"eof"}
```

---

## 4. 项目骨架导览

```
macast-gpui/
├── Cargo.toml                          # workspace
├── GUIDE.md                            # 本文件
├── README.md                           # 快速开始
└── crates/
    ├── macast-core/                    # 协议核心（无 UI，可单独测试）
    │   ├── assets/                     # SCPD + SinkProtocolInfo（来自 Macast, MIT）
    │   └── src/
    │       ├── lib.rs                  # 公共出口：MacastCore / Config / TransportState
    │       ├── config.rs               # 配置持久化（UDN、端口、播放器选择）
    │       ├── state.rs                # UPnP 状态变量表 + PlaybackSnapshot（给 UI）
    │       ├── device.rs               # description.xml 生成 + SCPD 内嵌
    │       ├── ssdp.rs                 # 组播发现 + 通告（socket2）
    │       ├── http.rs                 # axum 路由（description/scpd/action/event）
    │       ├── soap.rs                 # SOAP 解析/响应/错误
    │       ├── services/               # 三个服务的动作处理（对应 protocol.py）
    │       │   ├── avtransport.rs      #   SetAVTransportURI/Play/Pause/Stop/Seek/Get*
    │       │   ├── rendering_control.rs
    │       │   └── connection_manager.rs
    │       ├── eventing.rs             # GENA 订阅表 + LastChange 推送
    │       ├── core.rs                 # Engine/MacastCore：装配一切 + 事件桥
    │       └── player/
    │           ├── mod.rs              # Player trait（对应 Macast 的 Renderer 基类）
    │           ├── alhazen.rs          # alhazen 后端（默认，纯进程内解码，渲染走 gpui video_view）
    │           ├── libmpv.rs           # mpv 后端（进程内 libmpv + SW render → BGRA 帧）
    │           └── video_host.rs       # VideoHost trait（窗口命令边界）
    └── macast-app/                     # gpui 界面
        └── src/
            ├── main.rs                 # 启动 tokio runtime + gpui 窗口
            └── views/mod.rs            # RootView：状态/进度/控制/音量/复制链接
```

### 4.1 与原版的对应关系

| Macast (Python) | 本骨架 (Rust) |
|---|---|
| `Renderer` 基类 + `set_media_*` | `player::Player` trait + 同名方法 |
| `MPVRender`（spawn mpv + IPC） | `player::libmpv::LibmpvPlayer`（进程内 libmpv + SW render） |
| `DLNAProtocol.call()` 的 SOAP 分发 | `soap::parse_call` + `services::dispatch` |
| `DLNAProtocol` 状态变量表 | `state::RendererState.vars` |
| `ObserveClient` / `send_event_callback` | `eventing::EventRegistry` |
| `SSDPServer` | `ssdp::SsdpServer` |
| `Service`（cherrypy + AutoPort） | `http::router` + `MacastCore::start` 的端口回退 |
| pywebview 设置页 + pystray | gpui `RootView`（托盘见 §9） |

### 4.2 数据流：手机投一个视频进来

1. 手机 M-SEARCH → `ssdp.rs` 应答 → 手机 GET `/description.xml`
2. 手机 POST `SetAVTransportURI`（URL + DIDL 元数据）→ `services/avtransport.rs`
   → `player.set_media_url()` → mpv `loadfile`
3. 手机 POST `Play` → `player.resume()`
4. mpv IPC 事件 `time-pos/duration/pause` → `core.rs::bridge_loop`
   → 更新状态变量 → `eventing` 推 LastChange + 更新 UI 快照
5. UI 的 `watch` 通道收到新快照 → 重绘

---

## 5. 分阶段实施路线

骨架已经把 P0–P3 的主干写完了。建议按下面顺序**逐段读懂 + 逐段验证**，不要一次全改。

### P0 · 环境与跑通骨架（½ 天）

```bash
# 需要：Rust stable（已确认 1.96）+ mpv（Windows 装 mpv 并确保 mpv.exe 在 PATH，
# 或把路径写进 %APPDATA%/macast-gpui/config.json 的 mpv_path）
cd macast-gpui
cargo run -p macast-app
```
- 看到窗口显示"空闲"、IP、端口 → 骨架 OK
- **没有 mpv 也能跑**：把 config.json 的 `player` 改成 `"null"`，协议层全部可联调

验收：窗口起来；日志有 `SSDP: listening on N interface(s)` 和 `HTTP server listening on 0.0.0.0:PORT`。

### P1 · 让手机"看见"你（发现 + 描述）

读 `ssdp.rs`、`device.rs`。自测：
```bash
# Linux/macOS 可用 gssdp-discover；没有工具就用 nc 手发：
printf 'M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: "ssdp:discover"\r\nMX: 2\r\nST: ssdp:all\r\n\r\n' | nc -u 239.255.255.250 1900
```
或直接 `curl http://127.0.0.1:5069/description.xml` 看设备描述。

验收：手机上"投屏"列表出现 `Macast-Gpui(你的电脑名)`。

### P2 · 端到端播放（SOAP + mpv）

读 `services/avtransport.rs`、`player/`。手机推一个视频，确认：
- 画面出现在 gpui 窗口内（alhazen 走 `video_view()`，mpv 走 libmpv 渲出的帧）
- `GetPositionInfo` 返回的进度在走（手机端进度条动）
- 手机上的播放/暂停/停止/音量能反过来控制

照片同理：`SetAVTransportURI` 带 `upnp:class = object.item.imageItem.photo`（或 URI 以图片扩展名结尾）时，core 判定为图片、**不把 URL 交给播放后端**，由 UI 层 `MacastCore::fetch_bytes` 下载 + `image` 解码 + `paint_image` 画在窗口内；`Play` 只是把状态置为 PLAYING，`Stop` 清除画面。详见 §2.1 的 `MediaKind` 说明。

alhazen 后端（默认，`config.json` 的 `player.kind = "alhazen"`）是进程内解码 + gpui `video_view()` 内嵌渲染，视频画在 Now Playing 卡片里，不弹外部窗口。Windows 上走 Media Foundation DXVA2 硬件解码（默认适配器，Optimus 笔记本上即核显）。播放配置固定 `prefer_hardware=true` + `auto_fallback=false` + `frame_queue_len=16`：防止硬件解码稍慢就静默切到 ffmpeg-cli 软件解码，深队列吸收解码器突发帧。实测 1080p60 满帧 0 丢帧；4K60 受 alhazen-core 0.6.0 逐像素帧回传 + gpui sprite atlas 每帧整帧上传双重限制约 27fps（后续升级 alhazen 或本地 patch 可解）。

mpv 后端也已在窗口内渲染（进程内 libmpv + SW render → `paint_image`），**不需要 `mpv.exe`，但需要 `libmpv-2.dll`**；细节见 README「两种窗口内播放后端」与 `docs/plan-inwindow-mpv.md`（该计划已落地）。

诊断：设 `ALHAZEN_DEBUG=1` 环境变量启动，每秒打印 shown fps / paint 耗时 / 丢帧数 / 当前后端；应用日志里可见 `Media Foundation H264: Microsoft H264 Video Decoder MFT (GPU)`。

验收标准：推流 → 播放 → 暂停 → 进度条跟随 → 停止，全链路通。

### P3 · 事件推送完善（GENA）

读 `eventing.rs`。用手机投屏时观察：**不轮询**的情况下，手机上的播放状态/进度是否实时更新（这是 LastChange 推送的功劳）。Kodi/VLC 作为控制端时会订阅事件，可用它们验证。

验收：订阅者收到初始事件 + 状态变化事件（日志有 `GENA subscribe` / 投屏时事件发出）。

### P4 · UI 完善（gpui）

读 `macast-app/src/views/mod.rs`。界面已按"影音控制台"重做（2026-10）：

- **设计令牌**：深色 surface 阶梯（`BG #0A0D13` / `SURFACE #121722` / `SURFACE_HI #171E2C`）、单一强调色 `#6C8CFF`、语义色只用于状态（播放绿 / 暂停琥珀 / 待机灰）、文字三级（`TEXT` / `MUTED` / `DIM`）。
- **布局**：46px 自绘标题栏（品牌、状态、设置、最小化/最大化/关闭）+ Now Playing 卡片（封面图形、状态 chip、标题、进度、时间）+ 64px 悬浮控制栏 + 窄 footer。内容区 `max_w 1100 / max_h 440`，最大化时自动居中。
- **组件**：`icon_button` / `text_button` / `switch` / 分段控件 / toast，全部用 `div` + `canvas` 组合；图标是矢量路径（播放、暂停、停止、音量、静音、滑杆、关闭、最小化、最大化、10s 箭头、复制、投屏、均衡器）。
- **设置**：右侧滑出面板（348px）+ 半透明遮罩，点击面板内部不关闭（`cx.stop_propagation()`），点遮罩/关闭按钮关闭。
- **Windows 细节**：gpui 0.2.2 的 `start_window_move` 在 Windows 上是空实现，自绘标题栏拖动用 `WM_NCLBUTTONDOWN + HTCAPTION`；最大化/最小化/隐藏用 `ShowWindowAsync`，避免在事件处理里同步 `ShowWindow` 触发 `RefCell already borrowed`（会让 surface 不 resize）。
- 主题：`Config.theme`（`auto` / `light` / `dark`，默认 auto），渲染时用 `window.appearance()` 解析；`observe_window_appearance` 负责系统主题变化时重绘。设置里的分段控件即时生效，不需要重启。
- 后端展示：设置面板区分「当前运行」（`core.engine.player.name()`）与「启动后端」（`config.player`）；当启动配置是 mpv 但实际回退 null 时，给出琥珀色「mpv 回退」chip 和排查提示；footer 同步标记。
- 截图：`docs/screenshot-idle.png` / `docs/screenshot-playing.png` / `docs/screenshot-light.png`（Windows 150% 缩放实拍，抓取用 PrintWindow + 像素校验，不受遮挡影响）。
- 测试视频注意：供 mpv 经 HTTP 播放的样片**必须带 `-movflags +faststart`**（moov 在尾部时 mpv 探测失败，0.5s 内直接 end-file → 状态 Stopped）；测试音轨用 `anullsrc` 静音，避免截图/测试时出声。

可继续做：
- 应用内文本输入（gpui 0.2.2 没有 Input 元素，需自己处理 key_down + IME，工作量大）
- 字幕开关（`DisplayCurrentSubtitle` 状态变量已留好）
- 投屏历史 / 最近播放列表

### P5 · 产品化

- **托盘图标**（见 §9）
- **开机自启**：已实现 Windows 版本（设置面板开关，写 `HKCU\...\CurrentVersion\Run`）；macOS 需 LaunchAgent
- **打包**：见 §7
- **多语言/主题**：主题已实现（auto / light / dark）；多语言未做

---

## 6. 测试指南

### 6.1 用真手机测（最重要）

- **Android**：各视频 App 的"投屏/TV"按钮（一般是 DLNA）；系统相册的"投射"；或装 [BubbleUPnP](https://bubbleupnp.fabricemontfort.com/)/VLC
- **iOS**：支持 DLNA 的 App（如 VLC、Infuse、nPlayer；系统 AirPlay 不兼容 DLNA，见 §9）
- 推流时手机和电脑必须在**同一网段**（路由器 AP 隔离会挡住组播，见 §8）

### 6.2 用桌面控制端测（可看日志细节）

- **VLC**：视图 → 播放列表 → "Universal Plug'n'Play" 下应该出现你的设备
- **Kodi**：设置里开 UPnP/DLNA 渲染器控制
- 这两个客户端会正经走 SUBSCRIBE 事件订阅，是验证 GENA 的好工具

### 6.3 curl 手测 SOAP（调试神器）

```bash
# GetTransportInfo（应该返回 STOPPED 或 NO_MEDIA_PRESENT）
curl -i -X POST http://127.0.0.1:5069/AVTransport/action \
  -H 'SOAPACTION: "urn:schemas-upnp-org:service:AVTransport:1#GetTransportInfo"' \
  -H 'Content-Type: text/xml; charset="utf-8"' \
  --data '<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body><u:GetTransportInfo xmlns:u="urn:schemas-upnp-org:service:AVTransport:1"><InstanceID>0</InstanceID></u:GetTransportInfo></s:Body></s:Envelope>'

# SetVolume
curl -i -X POST http://127.0.0.1:5069/RenderingControl/action \
  -H 'SOAPACTION: "urn:schemas-upnp-org:service:RenderingControl:1#SetVolume"' \
  --data '<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><u:SetVolume xmlns:u="urn:schemas-upnp-org:service:RenderingControl:1"><InstanceID>0</InstanceID><Channel>Master</Channel><DesiredVolume>30</DesiredVolume></u:SetVolume></s:Body></s:Envelope>'

# GetProtocolInfo（Sink 列表是否正常返回）
curl -s -X POST http://127.0.0.1:5069/ConnectionManager/action \
  -H 'SOAPACTION: "urn:schemas-upnp-org:service:ConnectionManager:1#GetProtocolInfo"' \
  --data '<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><u:GetProtocolInfo xmlns:u="urn:schemas-upnp-org:service:ConnectionManager:1"/></s:Body></s:Envelope>'
```

### 6.4 无 mpv 联调（CI 友好）

`config.json` 设 `"player": "null"`，NullPlayer 会模拟 1Hz 进度事件，DLNA 全链路可自动化测试。

### 6.5 本骨架的实测记录（2026-10，Windows 真机）

以下全部在开发机上实测通过，可作为你复刻时的对拍基准：

| 验证项 | 方法 | 结果 |
|---|---|---|
| 配置生成 / mpv 缺失回退 | 无 mpv 环境直接运行 | 生成 `config.json`，自动回退 null player 并打日志 |
| 端口占用回退 | 手动占用 5069 后启动 | 自动落到随机端口，日志可见 |
| gpui 窗口 | 实机运行 | D3D11 渲染、字体加载、界面正常绘制 |
| 设备描述 / SCPD | `curl /description.xml`、`/dlna/*.xml` | MediaRenderer 描述 + 三个 SCPD 全部正确返回 |
| SOAP 控制 | curl 逐个动作 | Get/SetVolume、GetTransportInfo、GetMediaInfo、GetPositionInfo、GetProtocolInfo、SetAVTransportURI、Play、Pause、Seek 全部正确 |
| 状态流转 | Play/Pause 后查 GetTransportInfo | `NO_MEDIA_PRESENT → PLAYING → PAUSED_PLAYBACK` |
| DIDL-Lite 标题解析 | 推带 `dc:title` 的元数据 | 标题正确提取并出现在状态页 |
| GENA 订阅 | SUBSCRIBE + 本地回调服务器 | 返回 SID；初始全量 LastChange 正确；**SEQ 严格递增（0,1,2,…）** |
| **真机控制端投屏** | 局域网控制端自发发现 | 对方完成 SSDP 发现 → GENA 订阅 → SetAVTransportURI 推图，全链路通 |

> 实测中发现并修掉的两个 bug，值得记住：① axum 0.8 不允许路径段里"参数+字面量"混排（`/dlna/{service}.xml` 会直接 panic），SCPD 要注册显式路由；② GENA SEQ 如果只在 clone 出来的 Subscriber 上自增，每条 NOTIFY 都会是 SEQ 0，必须回写注册表。

**2026-10-19 增量验证**（UI 功能 + 测试）：

| 验证项 | 方法 | 结果 |
|---|---|---|
| 核心单元测试 | `cargo test -p macast-core` | 13 passed（时间解析、HTTP 日期、DIDL、SOAP、配置读写、mpv 空闲无窗口） |
| 应用编译 | `cargo check -p macast-app -j 2` / `cargo build` | 通过 |
| 启动冒烟 | 运行 `target/debug/macast-gpui.exe` | 窗口/HTTP/SSDP 正常；无 mpv 时自动回退 null 并打日志 |
| 托盘常驻 | 日志 `tray icon created`；用 Win32 消息模拟左键、关闭、菜单退出 | 关闭后窗口隐藏且进程存活；托盘点击恢复可见；退出触发 `shutting down MacastCore` |
| 应用图标 | `LoadImageW(exe, 1, IMAGE_ICON)` + `GetClassLongPtr(GCLP_HICON)` | 资源 ID 1 存在（16–256 px），窗口类图标句柄非 0 |
| mpv 空闲窗口 | 实机：启动后枚举 mpv 窗口；Play 本地 30s 测试视频；Stop | 空闲时 2 个隐藏窗口、0 个可见；Play 后 1 个窗口 **0,0,1707,960 全屏**；Stop 后 0 可见 |
| mpv 进程清理 | 托盘退出后检查进程 | 应用退出后 mpv.exe 一并结束（taskkill /T 结束 shim 进程树） |
| 投屏全屏 | 单测检查每次投屏的 mpv 命令序列 | `fullscreen=true` 在 `loadfile` 之前发送；`mpv_fullscreen=false` 可关闭 |
| GPU 加速 | 启动日志 | `Using GPU: Intel Iris Xe` + `Created device with Direct3D 11.1`（硬件 D3D11；`0x887A002D` 只是 debug layer 缺失） |
| 启动耗时 | 进程启动到主窗口可见（3 次） | **0.24–0.25s**（优化前 6.2s，见 §8 排查表） |
| UI 重设计（视觉） | 150% 缩放下截取 idle / playing / settings / 最大化 / 最小尺寸 | 布局无溢出，内容在 1100×440 内居中；截图见 `docs/` |
| 自绘标题栏交互 | 模拟鼠标：拖动、双击最大化/还原、点关闭 | 窗口可拖动；最大化后 surface 正确 resize；关闭隐藏到托盘 |
| 设置滑出层 | 点设置图标 → 点面板内部 → 点遮罩 | 打开正常；点面板内部不关闭（stop_propagation）；点遮罩关闭 |
| 提示条 | 点「复制链接」 | 底部 toast 出现并 4s 自动消失 |
| 主题 | 设置里依次点 自动/亮色/暗色，检查 `config.json` 与截图 | 即时换色、无需重启；auto 跟随系统（本机亮色）；持久化为 snake_case |
| 后端显示 | 检查 mpv 缺失时的设置/footer | 「当前运行 null + mpv 回退」+ 琥珀提示；装好 mpv 后显示 mpv |
| 点击 Seek / 拖拽播放 | 需在有图形会话的机器上人工点测 | 代码已编译通过，未在本机截图验证（当前会话桌面不可捕获） |

> 顺带修掉一个隐藏 bug：`http_date_now()` 的星期索引算错（1970-01-01 会输出 `Fri`），已改为 `(days + 4) % 7` 并补了单元测试。

### 6.5 Wireshark 过滤器（排障）

```
ssdp                    # 所有 SSDP 报文
http.request.method == "NOTIFY"   # GENA 事件推送
```

---

## 7. 打包与分发（Windows）

```bash
cargo build --release -p macast-app
# 产物：target/release/macast-gpui.exe
```

### 7.1 安装包（cargo-packager）

用 [crabnebula-dev/cargo-packager](https://github.com/crabnebula-dev/cargo-packager) 打 Windows 安装包：

```powershell
cargo install cargo-packager --locked          # 一次性安装 CLI（≥0.11）
powershell -ExecutionPolicy Bypass -File scripts\installer.ps1
# 产物：dist\macast-gpui_0.1.0_x64-setup.exe
```

- 配置在 `crates/macast-app/Cargo.toml` 的 `[package.metadata.packager]`（`product_name` / `identifier` /
  `icons` / `out_dir`）与 `[package.metadata.packager.nsis]`（`installer-icon`、`languages`、
  `display-language-selector`）。**注意**：顶层 `name` 必须等于 Cargo 包名（或干脆不写），
  写了别的值会让 cargo-packager 认为"没有可打包的配置"。
- 默认 `installer-mode = currentUser`：装到 `%LOCALAPPDATA%\Macast-Gpui`，**不弹 UAC**；模板自带桌面 +
  开始菜单快捷方式与卸载项，`license_file` 会把 LICENSE 一并装进目录。
- NSIS 由 cargo-packager 首次运行时自动下载（缓存在 `%LOCALAPPDATA%\.cargo-packager\NSIS`），
  无需单独安装；`-Msi`（WiX）才需要 WiX Toolset。
- 已验证：解包载荷含 `macast-gpui.exe` + `uninstall.exe` + LICENSE，图标与中英双语选择正常。

> release 构建带 `windows_subsystem = "windows"`（由 `main.rs` 顶部的 `cfg_attr` 控制）：双击不弹终端，日志写 `%APPDATA%\macast-gpui\logs\macast-gpui.log`。debug 构建仍保留控制台，方便 `cargo run` 看日志。

随包分发清单：
1. `macast-gpui.exe`
2. （可选）`libmpv-2.dll` —— 仅 mpv 后端需要；放 exe 同目录或 `PATH`，或用 `player.libmpv_path` 指定。只用 alhazen 时可完全不打包（**推荐**，DLL 约 121 MB）
3. 首次运行会写 `%APPDATA%\macast-gpui\config.json`（UDN 在这里，**别删**，删了手机会当成新设备）

注意：
- **防火墙**：首次运行 Windows 会弹"允许访问专用网络"，必须允许，否则 SSDP 应答和 HTTP 都出不去
- mpv 后端的画面来自进程内 libmpv，不再有外部窗口；找不到 `libmpv-2.dll` 时回退 alhazen
- 想静默安装可配 NSIS/WiX，但记得处理防火墙规则（`netsh advfirewall`）

---

## 8. 常见坑与排查表

| 症状 | 原因 | 处理 |
|---|---|---|
| 手机列表里根本没有我 | AP 隔离 / 防火墙 / 不同网段 / 用了 5GHz 访客网络 | 同网段；看 Windows 防火墙日志；关 AP 隔离 |
| 能发现但推流没反应 | `POST /AVTransport/action` 404 或 SOAP 命名空间不匹配 | `curl` 直测（§6.3）；看日志有没有 dispatch 到动作 |
| 手机显示"不支持该格式" | `GetProtocolInfo` 的 Sink 列表太窄 | 用 Macast 的 csv（已内嵌）；或按需增删 mime |
| 进度不走 / 手机进度条不动 | 播放器事件没回传（mpv 的事件循环 / alhazen 状态同步） | 日志看 `mpv backend ready`（mpv）；用 curl 直测 `GetPositionInfo`（§6.3） |
| 多网卡时部分设备搜不到 | M-SEARCH 应答从错误网卡出去 | `ssdp.rs` 已按子网选网卡；虚拟机/Hyper-V 虚拟网卡可能干扰，可在 `interfaces()` 过滤 |
| 换 IP 后手机还连旧地址 | SSDP 缓存（max-age）未过期 | 重启 app 重发 byebye+alive；或调小 `ssdp_notify_secs` |
| 端口 1900 被占用 | 系统里已有 SSDP 服务 | Windows 下 `SO_REUSEADDR` 可共存；不行就排查其它投屏软件 |
| 编译 gpui 失败 | 缺 VS C++ 生成工具 / 旧版 Rust | 装 "Desktop development with C++"；用最新 stable |
| release 报 `Failed to find fxc.exe` | gpui 只查 PATH 和固定版本 SDK | 设 `GPUI_FXC_PATH=...\Windows Kits\10\bin\<ver>\x64\fxc.exe` |
| 双击启动卡 5–6 秒 | 首帧前同步 spawn 子进程（原 `is_autostart_enabled` 调 `reg.exe`），被杀软/系统拖慢 | 改用 `windows-registry` 直读注册表；不要在 `RootView::new` 里 spawn 进程 |
| mpv 后端起不来 | 找不到 `libmpv-2.dll` | 放 exe 同目录 / `PATH`，或用 `player.libmpv_path` 写绝对路径；看启动日志（找不到时会回退 alhazen） |

### 8.1 gpui 0.2.2 API 备忘（与 Zed 主仓源码有差异，本骨架已验证）

crates.io 上的 `gpui 0.2.2` 是**裁剪版**，和 Zed 主仓的 gpui 不是同一套 API。照 Zed 源码或教程写会编译不过，以下是实战踩坑记录：

| 你想写的 | gpui 0.2.2 的实际情况 | 写法 |
|---|---|---|
| `Button::new("id").label("x").on_click(...)` | **没有 `Button` 组件**（crates.io 版只发布了 div/text/img/list 等基础元素） | 用 `div().id("id").cursor_pointer().on_click(...)` 自制（见 `views/mod.rs` 的 `button()`） |
| `cx.spawn(\|view, cx\| async move {...})` | edition 2024，`spawn` 要的是 **async 闭包**，且推断不出参数类型会报 E0282 | `cx.spawn(async move \|view, cx\| {...})`（普通闭包+async 块不行） |
| `view.update(&mut cx, ...)` | async 闭包里 `cx` 已是 `&mut AsyncApp`，再取址会得到 `&mut &mut AsyncApp`（不实现 `AppContext`） | `view.update(&mut *cx, ...)` 重新借用 |
| `div().overflow_y_scroll()` | `overflow_*_scroll` 在 `StatefulInteractiveElement` 上，普通 `Div` 没有 | 先 `.id("...")` 得到 `Stateful<Div>` 再调 |
| `WindowBounds::centered(size(780., 560.), cx)` | 参数要 `Size<Pixels>` 而非 `Size<f32>` | `size(px(780.0), px(560.0))` |
| `cx.on_app_quit(\|_\| {...})` | 回调必须**返回 Future**，且是 `FnMut`（不能 move 捕获变量） | `cx.on_app_quit(move \|_\| { let h = handle.clone(); async move {...} })` |
| `WindowOptions { window_bounds, ..Default::default() }` | ✅ 可用；`cx.activate(true)` / `cx.open_window` 与旧版一致 | — |
| 样式方法 `p_6() gap_5() rounded_xl() text_sm() flex_1() border_1()` | ✅ 全部存在（宏生成） | — |
| `cx.listener(\|this, event, window, cx\| ...)` | ✅ 存在，返回 `impl Fn(&E, &mut Window, &mut App)`，正好喂给 div 的 `on_click` | — |

另一个环境问题：本机并行编译 gpui 依赖树时 rustc 会成片崩溃（`STATUS_STACK_BUFFER_OVERRUN`），**`cargo check -j 2`（或 `-j 1`）即可绕过**，与代码无关。

---

## 9. 进阶方向

按投入产出排序：

1. **托盘图标 + 关闭到托盘**（已实现 Windows）：`crates/macast-app/src/tray.rs` 用 `tray-icon` 建图标（运行时生成 32×32 RGBA，无需图片资源），菜单「显示主窗口 / 退出」。gpui 的 Windows 消息循环是 `GetMessageW + DispatchMessageW`，会顺带把 tray-icon 隐藏窗口的消息派发掉，所以不用自己起事件循环。窗口隐藏/恢复通过 `raw-window-handle` 拿 HWND，再调 `ShowWindow(SW_HIDE/SW_SHOW)`；用 `Window::on_window_should_close` 返回 `false` 拦截关闭。macOS 用 `objc2` 建 `NSStatusBar`。
2. **设置界面**：友好名、端口、播放器后端、libmpv 路径、字幕/倍速，全部落 `config.rs`
3. **播放器插件化**：把 `Player` trait 的具械做成"渲染器插件"（对应 Macast-plugins 的思路：IINA/PotPlayer/外部命令），用 dlopen 或子进程 + 简单 RPC
4. **AirPlay 接收**：协议完全不同（mDNS + FairPlay + NTP），工作量大；可评估集成 `rust-airplay` 类项目或 fork Macast 的音频方案
5. **Chromecast**：mDNS + CASTV2 over TLS，更重；通常不值得从零做
6. **视频嵌入 gpui 窗口**（已实现）：crates.io gpui 0.2.2 的 `surface` 元素 Windows 上是空壳、子 HWND 会被 DirectComposition 盖住，最终走 **解码帧 → `RenderImage` → `paint_image`**——alhazen 后端（默认）与进程内 libmpv 后端都用这条路
7. **投屏历史 / 自动续播**：`GetPositionInfo` 已有数据，UI 层加个列表即可

---

## 10. 参考资料

- [UPnP Device Architecture 1.1](http://upnp.org/specs/arch/UPnP-arch-DeviceArchitecture-v1.1.pdf)（SSDP/GENA/SOAP 都在里面）
- [UPnP MediaRenderer:1 Device](http://upnp.org/specs/av/UPnP-av-MediaRenderer-v1-Device.pdf)
- [AVTransport:1 Service](http://upnp.org/specs/av/UPnP-av-AVTransport-v1-Service.pdf)
- [RenderingControl:1 Service](http://upnp.org/specs/av/UPnP-av-RenderingControl-v1-Service.pdf)
- [ContentDirectory:1 Service](http://upnp.org/specs/av/UPnP-av-ContentDirectory-v1-Service.pdf)（投屏源端，了解即可）
- Macast 源码（本指南 §1 的行为细节全部来自它）：
  - `macast/ssdp.py`、`macast/protocol.py`、`macast/server.py`、`macast/renderer.py`、`macast_renderer/mpv.py`
- [python-upnp-ssdp-example](https://github.com/ZeWaren/python-upnp-ssdp-example)（最小 SSDP 实现，半小时可读完）
- mpv 手册：[JSON IPC](https://mpv.io/manual/master/#json-ipc)、[input commands](https://mpv.io/manual/master/#list-of-input-commands)
- gpui：[crates.io/gpui](https://crates.io/crates/gpui)（0.2.2）、[Zed 的 gpui 源码](https://github.com/zed-industries/zed/tree/main/crates/gpui)

---

*骨架代码 + 本指南即最小可复刻集。先把 P1–P3 跑通，你就拥有了一个比原版更 Rust 化的 Macast。*
