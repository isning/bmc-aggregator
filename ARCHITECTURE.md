# BMC Aggregator 架构

BMC Aggregator 使用 Rust/Axum 聚合 BMC，Vue 前端通过设备作用域 API 访问功能。
本文描述当前模块职责；配置见 [配置参考](docs/configuration.md)，验证见
[测试说明](docs/testing/README.md)。

## 调用方向与模块职责

HTTP/WS 客户端进入 Feature 层，经 `src/bmc/backends/` 薄适配器调用独立协议 crate。
返回的事件、状态和原始帧通过适配器进入 Feature runtime。

- `src/web/`：根路由、静态页面、dummy auth/setup、设备级入口和服务配置。
- `src/bmc/feature/`：功能路由、生命周期、状态、故障与浏览器会话。
- `src/bmc/configuration/`：配置目录、事务、版本、幂等重放与配置事件。
- `src/bmc/backends/`：配置映射、typed runtime 安装、事件及帧桥接。
- `crates/bmc-feature-api/`：稳定的功能契约。
- `crates/bmc-backend-support/`：协议无关的辅助实现。
- `crates/bmc-backend-*/`：协议、连接池、传输、编解码与 typed trait 实现。
- `crates/bmc-idrac-model/`、`crates/bmc-backend-model/`：纯状态决策模型。
- `frontend/src/features/`：功能 UI；`frontend/src/components/`：共享 UI。
- `contracts/`：生成前后端功能配置 DTO 的 schema。

协议 backend crate 之间不互相依赖，也不依赖主应用 crate。应用适配器不拥有
socket、HTTP client、TLS、协议解析、连接池或功能 trait 的协议实现。

## 生命周期与运行时

`FeatureConnection` 是功能生命周期句柄，拥有身份、配置、连接状态、引用计数、
事件和两个私有 runtime slot。它不是一条物理 BMC 连接。

- `backend_runtime`：通过 typed accessor 取得；由 `connect(fc)` 安装，重连时清理。
- `feature_runtime`：Feature 拥有的状态，跨 backend 重连保留。
- `FeatureGuard`：RAII 引用计数，保持使用中的功能存活。

连接任务处理连接、断开、失败、空闲和停止；协议内的控制决策由纯模型处理。
应用 `FeatureConnection` 生命周期本身没有迁入协议模型。
Mahogany、VNC、AVCT 的池可以在相应功能之间共享物理传输，功能生命周期仍各自独立。

纯模型必须无依赖、`no_std`，只接收 owned state/event 并返回 state/effect。
时间、完成、超时与远端观察由 runtime 显式输入。I/O、资源所有权与像素变换保留在
runtime。未知结果不等于成功；队列接纳不等于设备确认。
改动前阅读 [iDRAC ADR](docs/adr/0015-pure-idrac-state-machines.md) 或
[其他后端 ADR](docs/adr/0016-pure-backend-state-machines.md)。

## 功能注册

`src/bmc/feature/mod.rs` 是注册事实来源；`FeatureBackendTemplate` 包含 backend、
feature、connect，以及可选的 `BackendRequirementHooks`。

| Backend | Features |
|---|---|
| mahogany | video, hid, msd, firmware |
| vnc | video, hid |
| idrac-avct-console | video, hid |
| idrac-legacy-media | msd |
| idrac-wsman | boot |
| ipmi | power, boot, identify |
| redfish | power, boot, identify, msd, firmware, health |
| wol | power |

Audio 是明确不可用的占位功能。注册表示已有实现，不保证目标设备提供能力或授予权限。
Boot 可配置多个有序 provider；provider 的协议实现相互独立，不做跨 backend 自动重试。
旧 YAML 推断仅发生在 `DeviceConfig::normalize_feature_config()`，运行时使用显式分配。

新增后端时，先在独立 crate 实现稳定 trait，再增加应用薄适配器和显式注册。
配置优先使用 `backend_params` / `feature_params`，避免新增顶层兼容字段。

## 视频与输入

`VideoSource` 只提供请求完整帧和 source trace。协议 crate 生产原始帧，应用适配器
转发给 `VideoRuntime`。后者持有跨重连的 `VideoProcessor`、信号状态与事件绑定。

`VideoProcessor` 拥有编码选择、码率、约束、自检、原始帧广播、末帧重放、MJPEG
计时及 WebRTC/WebTransport 会话。后端不管理浏览器编码或会话。

持久化视频配置由 `video/stream_config.rs` 进入公共配置事务，保存后应用到功能。
运行时流控制和持久化配置是不同操作；默认编码、采集参数、最大 FPS 持久化在
`features.video.feature_params`。

HID 路由调用 typed `HidBackend`，协议 crate 执行键盘、鼠标和 reset。
输入接纳、写入完成和远端结果分别处理；断网后不能保证远端已释放输入。

## 其他功能

MSD 的公共模型区分 `aggregator`、`bmc`、`remote` 存储位置，以能力描述操作和
媒体类型。应用管理文件及 HTTP 操作，backend 实现协议和设备确认。
详见 [媒体模型](docs/architecture/msd-media-model.md)。

Power、Boot、Identify、Firmware、Health 使用各自 typed 契约。WOL 发包不能证明
设备已开机；IPMI 接受命令不能证明电源转换完成；Redfish 任务完成仍需相应读回。
故障边界见 [功能故障模型](docs/architecture/feature-fault-model.md)。

## 路由与配置边界

- 根服务、认证兼容、设置及配置 API：`/api/*`。
- 设备信息、状态、事件、需求等：`/bmc/:id/api/*` 中的设备级路由。
- 功能运行时和配置：`/bmc/:id/api/feature/<feature>/*`。

`mount_all_feature_routes()` 挂载 video、hid、msd、power、audio、boot、identify、
firmware、health。旧设备级 `/api/config*` 和认证、系统、流等兼容别名不挂载。
具体方法与路径以功能模块和 `src/web/routes.rs` 为准。

设备设置使用 `deviceConfiguration.ts` 和服务端配置目录；控制台保留 video、hid、
audio 功能配置 store；全局设置拥有自己的服务请求状态。没有中央 config store。
配置事务提供 ETag、mutation ID、脱敏读取和修订事件，细节见配置参考。

## 构建与验证

```bash
nix develop --command just build
nix develop --command just ci
```

`just ci` 运行 Rust、前端及工具检查；也可分别执行 `ci-rust`、`ci-frontend`、
`ci-tools`。纯模型边界和完整源码清单由可执行检查约束。
CI 不替代真实 BMC、浏览器和部署验收；dummy auth 尚不提供真实用户授权。
