# 平台支持与能力矩阵

com0com-rust 在 Windows 上使用 Rust 内核驱动，在 Linux/macOS 上使用纯 Rust 用户态
PTY 后端。两套实现共用同一份 `com0com-core` 契约、SDK 门面、C ABI 与 JSON-RPC
协议，但**能力不同**：UI 必须先读 `Sdk::capabilities()`，只展示后端真正支持的项。

## 平台总览

| 平台 | 内核形态 | 后端 crate | `KernelKind` | 说明 |
|---|---|---|---|---|
| Windows 10/11 | KMDF 总线 + WDM 端口驱动 | `com0com-backend-win` | `WindowsKmdfWdm` | `CreateFile("COMx")` 与 Win32 Comm API 透明可用 |
| Linux | 无内核模块，用户态 PTY | `com0com-backend-pty` | `UnixPty` | 需要长驻进程（`com0comd`）持有 PTY |
| macOS | 无 kext/dext，用户态 PTY | `com0com-backend-pty` | `UnixPty` | 同上；Intel 与 Apple Silicon 行为一致 |
| 其他 | 无实现 | `com0com-backend-stub` | `Unknown` | 所有操作返回 `Unsupported` |

Unix 不实现 TTY 内核模块、DriverKit dext 或 kext，也不需要关闭 SIP。代价是它不是
"真正的串口驱动"：依赖 udev/IOKit 枚举设备名的应用看不到这些端口，按路径打开的
应用（pyserial、minicom、多数测试工具）可以正常使用。

## 能力矩阵

| 能力位 | Windows 内核 | Unix PTY | Stub | 说明 |
|---|---|---|---|---|
| `PAIR_MANAGEMENT` | 支持 | 支持 | — | 增删查对、自动/显式 id |
| `PARAM_BAUD_EMU` | 支持 | 支持 | — | Unix 按目标端 `cfgetospeed` 做用户态节流 |
| `PARAM_NOISE` | 支持 | 支持 | — | 逐字节翻转一位，`noise_probability` 0..=10000 |
| `PARAM_OVERRUN` | 支持 | 支持 | — | Unix 仅丢字节；不产生 `TTY_OVERRUN` 统计 |
| `PARAM_EXCLUSIVE` | 支持 | **false** | — | PTY 无法可靠拦截其他进程 open |
| `PARAM_PLUGIN` | 支持 | **false** | — | 无插件模型 |
| `PARAM_PIN_WIRING` | 支持 | **false** | — | PTY 没有真实 modem 线 |
| `SERIAL_MONITOR` | 支持 | 支持 | — | Unix 通过受管理的稳定链接打开 |
| `DRIVER_DIAGNOSTICS` | 支持 | **false** | — | Unix 无驱动；`driver_info` 只报 PTY 版本 |
| `DRIVER_LOGS` | 支持 | **false** | — | Unix 用 `com0comd --log` 的 daemon 日志 |
| `STATE_PERSISTENCE` | 支持 | 支持 | — | 两者都由 SDK/daemon 提供快照与恢复 |
| `EVENTS` | 支持 | 支持 | — | Unix 事件来自后端事件总线 |
| `REMOTE_DAEMON` | 支持 | 支持 | — | 本机 named pipe / Unix socket |

Unix 上 `exclusive`、`plugin` 或非 `NULL_MODEM` 的 pin wiring 会返回
`SdkError::Unsupported`，不会静默忽略或伪装成功。

## 端口路径

### Windows

`setupc add --port-a COM15 --port-b COM16` 创建 `COM15`/`COM16` 两个设备节点，
与内核驱动创建的命名一致。端口名最多 31 个 UTF-16 单元。

### Linux

| 运行身份 | 链接目录 | 端口路径 |
|---|---|---|
| root（推荐 daemon） | `/dev/com0com` | `/dev/com0com/<label>` |
| 普通用户 | `$XDG_RUNTIME_DIR/com0com` 或 `~/.local/state/com0com/dev` | `<dir>/<label>` |

`<label>` 默认是 `A<id>` / `B<id>`，也可以通过 `--port-a` / `--port-b` 指定；
只允许 `[A-Za-z0-9._-]`，长度 1..=31。稳定链接指向会变化的 `/dev/pts/N`。

### macOS

| 运行身份 | 链接目录 | 端口路径 |
|---|---|---|
| root（推荐 daemon） | `/dev` | `/dev/cu.com0com-<label>`，另有 `/dev/tty.com0com-<label>` 别名 |
| 普通用户 | `~/Library/Application Support/com0com/ports` | `<dir>/cu.com0com-<label>` |

macOS 上 `cu.*` 是非阻塞语义，`tty.*` 是阻塞语义；同一 PTY 从端两者都可用。

### 环境变量

| 变量 | 作用 |
|---|---|
| `COM0COM_DEV_DIR` | 覆盖链接目录（测试隔离、无权限部署） |
| `COM0COM_DEVICE_UID` / `COM0COM_DEVICE_GID` | root daemon 创建 PTY 后把从端 chown 给桌面用户 |
| `COM0COM_ENDPOINT` | 覆盖 daemon endpoint（socket 路径）；`Sdk::open()` 自动探测时也用它 |
| `COM0COM_BACKEND` | `native` 强制进程内 PTY，`daemon` 强制连接 daemon；未设置时自动探测 |
| `COM0COM_STATE` | 覆盖状态文件路径（daemon 与 SDK 共用） |
| `COM0COM_DAEMON` | Windows：非空时 `setupc` 走 daemon 而不是同进程 IOCTL |

## 进程生命周期（Unix 的关键差异）

PTY 对的生命周期绑定在**创建它的进程**上：进程退出，master/slave 关闭，链接失效。
因此 Unix 上不要让 `setupc` 常驻创建端口；应启动 `com0comd`，由它持有 PTY：

```bash
com0comd run --restore --autosave --log /var/log/com0com/com0comd.log &
setupc add --id 0 --port-a ttyUSB-A --port-b ttyUSB-B
```

`Sdk::open()` 在 Unix 上会先尝试默认 daemon（250 ms 超时），失败才回退到进程内 PTY；
`Sdk::open_native()` 强制进程内模式（仅用于短生命周期测试），
`Sdk::connect_daemon(None)` 强制 daemon 模式。

## 部署

### Linux（systemd）

示例单元：[`packaging/linux/com0comd.service`](../packaging/linux/com0comd.service)。

```bash
sudo install -m 0755 target/release/com0comd /usr/local/bin/com0comd
sudo install -m 0644 packaging/linux/com0comd.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now com0comd
```

单元会创建 `/run/com0com`（`0700`）、`/var/lib/com0com`、`/var/log/com0com`。状态文件
`0600`。默认 socket 权限 `0600`（仅 root）；要让桌面用户连接，在 `ExecStart` 后追加
`--allow-uid <uid>`（socket 变为 `0660`）并让 socket 目录可被目标组遍历，
例如 `Group=com0com` + `RuntimeDirectoryMode=0750`；每个连接再用 `SO_PEERCRED` 校验 uid。

### macOS（launchd）

示例 plist：[`packaging/macos/org.com0com.daemon.plist`](../packaging/macos/org.com0com.daemon.plist)。

```bash
sudo mkdir -p "/Library/Application Support/com0com" /Library/Logs/com0com
sudo install -m 0755 target/release/com0comd /usr/local/bin/com0comd
sudo install -m 0644 packaging/macos/org.com0com.daemon.plist /Library/LaunchDaemons/
sudo launchctl load -w /Library/LaunchDaemons/org.com0com.daemon.plist
```

launchd 不会自动创建状态/日志目录，必须先手工创建。普通用户模式不需要 root：
直接以该用户运行 `com0comd run`，端口、socket 与状态文件都会落在用户目录（`0700`）；
root 模式默认使用 `/dev`、`/var/run/com0com/control.sock` 与
`/Library/Application Support/com0com/state.json`。

### 安全边界

- daemon **只监听本机 IPC**：Windows named pipe 或 Unix domain socket，没有 TCP/UDP。
- Unix socket 默认 `0600`；`--allow-uid` 非空时 `0660` 并逐连接校验 peer uid。
- `serial.open` 只接受后端管理的端口查询结果，不接受调用方提供的任意设备路径。
- 链接目录中的普通文件永远不会被覆盖；只有指向 PTY 节点的旧符号链接会被替换。
- 状态文件采用同目录临时文件 + `sync_all` + rename 原子写入，文件 `0600`；
  非 root 的状态目录 `0700`。

## 限制与已知差异

- **枚举不到端口**：应用若通过 udev/IOKit 或驱动服务名查找设备，看不到 PTY 端口；
  请让用户按路径选择，或用 SDK 的 `list_ports()`。
- **termios 由应用决定**：PTY 创建时已 `cfmakeraw` + `CLOCAL|CREAD`，但应用之后仍可
  改回行缓冲/回显；E2E 用原始模式验证二进制透明。
- **无语义化 modem 线**：`TIOCMGET`/`TIOCMSET` 在 PTY 上不可靠，pin wiring 能力位为 false。
- **无 `TTY_OVERRUN` 统计**：overrun 只丢字节，统计口径与 Windows 内核不同。
- **PTY 缓冲有限**：内核缓冲远小于 Windows 环缓冲；持续高速写入且对端不读时会丢数据
  （开启 `overrun` 时按 500 ms 超时主动丢弃并计数）。
- **声音/噪声**：噪声是确定性 Xorshift32，按概率翻转**单个位**；不是线路噪声模型。
- **macOS 符号链接**：少数只接受真实 `/dev/cu.*` 的应用可能拒绝链接；可改用用户目录
  路径或直接通过 SDK Monitor 访问。

## 端到端验收

Windows 走 `tools/vm-test/run-vm-test.ps1`；Linux/macOS 使用
[`tools/vm-test/guest/check-pairs-unix.sh`](../tools/vm-test/guest/check-pairs-unix.sh)：

```bash
cargo build -p com0com-setupc -p com0com-daemon -p com0com-sdk-smoke
mkdir -p target/vm-test-results
tools/vm-test/guest/check-pairs-unix.sh \
  --base-dir target/debug \
  --log "target/vm-test-results/unix-$(date -u +%Y%m%d-%H%M%S)/check-pairs-unix.log"
```

脚本在临时目录内设置 `COM0COM_DEV_DIR` / `COM0COM_ENDPOINT` / `COM0COM_STATE`，不会
污染系统 `/dev` 或用户目录。CI 中该 job 仅在手动触发且仓库变量 `RUN_UNIX_E2E=true`
时运行；Windows VM 回归仍是发布前必跑项。
