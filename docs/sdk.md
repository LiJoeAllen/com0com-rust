# Rust SDK

`com0com-sdk` 是 UI/业务代码唯一需要依赖的门面。它按平台选择后端：

| 平台 | 原生后端 | 说明 |
|---|---|---|
| Windows | `com0com-backend-win` | SetupAPI + `DeviceIoControl`，操作 `com0combus`/`com0comport` |
| Linux / macOS | `com0com-backend-pty` | 纯 Rust 用户态 PTY |
| 其他平台 | `com0com-backend-stub` | 所有操作返回 `Unsupported` |

也可以显式连接本机守护进程（JSON-RPC），进程内无需管理员权限。

## 引入

```toml
[dependencies]
com0com-sdk = { path = "crates/com0com-sdk" }

# 单元测试用内存后端（不需要驱动）
[dev-dependencies]
com0com-sdk = { path = "crates/com0com-sdk", features = ["fake"] }
```

## 打开后端

```rust
use com0com_sdk::{BackendKind, Sdk};

// 当前平台的自然后端
let sdk = Sdk::open()?;

// 本机 daemon（endpoint 为 None 时使用平台默认值）
let sdk = Sdk::connect_daemon(None)?;

// 显式选择
let sdk = Sdk::builder()
    .backend(BackendKind::Daemon)
    .endpoint(r"\\.\pipe\com0comd-v1")
    .open()?;

// 测试：注入内存后端
let sdk = Sdk::with_backend(std::sync::Arc::new(com0com_sdk::FakeBackend::new()));
```

`Sdk` 是 `Send + Sync`，可以放进共享状态；`open_monitor` 返回的
[`Monitor`] 自身不是 `Sync`，每个 UI 线程各自持有一个更简单。

## 串口对

```rust
use com0com_sdk::{PairParams, PortConfig, PortName};

let pair = sdk.create_pair(None, &PairParams::default())?; // None = 自动 id
let pair = sdk.create_pair(Some(5), &params)?;             // 指定 id 0..=63

for pair in sdk.list_pairs()? {
    println!("pair {} {} <-> {}", pair.id, pair.params.port_a, pair.params.port_b);
}

let params = sdk.get_params(pair.id)?;
sdk.set_params(pair.id, &params)?;
sdk.delete_pair(pair.id)?;
```

`PairParams` 字段与校验：

| 字段 | 说明 |
|---|---|
| `port_a`, `port_b` | 端口名；空串表示由后端自动分配（Unix 回填 `A{id}` / `B{id}`） |
| `baud_emu` | 波特率时序仿真（写侧限速） |
| `noise_enabled`, `noise_probability` | 噪声注入；概率单位 0.01%，范围 `0..=10000` |
| `overrun` | 对端队列满时按溢出丢字节并计数 |
| `exclusive` | 独占打开（仅 Windows 内核驱动支持） |
| `plugin` | 插件模式（仅 Windows） |
| `pin_wiring_a`, `pin_wiring_b` | 引脚映射（仅 Windows） |

不支持的能力位为 `false`；SDK 不会静默伪造参数。

## 端口与监视器

```rust
let ports = sdk.list_ports()?;
for port in ports {
    println!("{} managed={} pair={:?}", port.name, port.managed, port.pair_id);
}

let name = PortName::new(&ports[0].name)?;
let mut monitor = sdk.open_monitor(&name, &PortConfig::default())?;

monitor.write(b"hello")?;
match monitor.recv_timeout(std::time::Duration::from_secs(1))? {
    MonitorEvent::Data(bytes) => { /* ... */ }
    MonitorEvent::Error(message) => eprintln!("{message}"),
    MonitorEvent::Closed => {}
}
println!("dropped chunks: {}", monitor.dropped());
```

`PortInfo.name` 在 Windows 上是 `COMx`，在 Unix 上是稳定的链接路径（如
`/dev/com0com/A0`），两者都可直接传给 `PortName::new`。31 字符上限只约束
`PairParams` 里保存的 pair 名称；Unix 链接路径可以更长。

`Monitor` 队列有界；读得慢时事件被丢弃并计入 `dropped()`，不会阻塞后端。

## 事件

```rust
use com0com_sdk::{Event, EventFilter, EventKind};

let events = sdk.subscribe(EventFilter::kinds(&[
    EventKind::PairAdded,
    EventKind::PairRemoved,
    EventKind::ParamsChanged,
]))?;

for event in events.events().try_iter() {
    match event {
        Event::PairAdded { pair } => { /* ... */ }
        Event::PairRemoved { id } => { /* ... */ }
        Event::ParamsChanged { pair } => { /* ... */ }
        Event::DriverStatusChanged { driver, status } => { /* ... */ }
    }
}
```

多个订阅互不影响，各自有独立过滤器与队列。Windows 后端通过轮询
`ENUM_PAIRS` 产生事件，因此外部进程（如 `setupc`）的修改同样可见。

## 能力位

```rust
let caps = sdk.capabilities();
if caps.contains(Capabilities::PARAM_PIN_WIRING) {
    // 显示引脚映射 UI
}
for (name, _) in caps.iter() {
    println!("{name}");
}
```

UI 必须根据能力位隐藏不可用功能。完整矩阵见 [platforms.md](platforms.md)。

## 状态持久化

```rust
let state = sdk.state();               // 平台默认状态文件

let snapshot = state.save()?;          // 原子写入，返回写出的快照
let json = state.export_json()?;       // 导出 JSON（schema = 1）
state.import_json(&json, RestoreOptions::default())?;

let report = state.restore(
    &snapshot,
    RestoreOptions { dry_run: false, remove_extra: false, continue_on_error: true },
)?;

assert!(report.is_success());
```

- schema 固定为 `1`，加载时校验，不匹配返回 `SdkError::Protocol`。
- 写入使用临时文件 + 原子替换，崩溃不会留下半截文件。
- 连接 daemon 时，`state()` 自动路由到 daemon 拥有的状态文件。

## 错误

所有 API 返回 `Result<_, SdkError>`：

```rust
pub enum SdkError {
    InvalidArgument(String), NotFound, AlreadyExists, Busy, PermissionDenied,
    Unsupported, DriverUnavailable, Io(String), Timeout, DaemonUnavailable,
    Protocol(String), Win32 { code: u32, message: String }, Internal(String),
}
```

`Unsupported` 表示当前后端没有该能力，不是临时错误。

## 诊断

```rust
let info = sdk.driver_info(DriverKind::Bus)?;
println!("{} {:?} {}", info.service_name, info.status, info.version.unwrap_or_default());

for entry in sdk.driver_logs(50)? {
    println!("{} {:?} {}", entry.timestamp, entry.level, entry.message);
}
```

Windows 上 `driver_info` 读取 SCM 服务状态、映像版本与 SHA-256；
`driver_logs` 读取 `System` 事件通道。PTY 后端不提供驱动诊断（能力位为 `false`）。

`com0comport` 是按需启动的功能驱动：没有任何 pair/端口设备时服务为
`Stopped`，创建首个 pair 后才会 `Running`。因此驱动状态检查应在 pair 存在时做；
无设备时的 `Stopped` 不是错误。Windows 的自动端口名由 PnP 分配，`get_params`
会保留空串（自动标记），实际 `COMx` 名字以 `list_ports()` 为准；Unix PTY 则
会把 `A{id}` / `B{id}` 回填进参数。
