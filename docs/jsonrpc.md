# 本机 JSON-RPC 守护进程（`com0comd`）

`com0comd` 把后端能力以 JSON-RPC 2.0 暴露在本机 IPC 上。UI 可以常驻一个
非特权进程连接它，避免每次管理操作都弹 UAC。**没有网络监听**：Windows 使用
named pipe，Unix 使用 Unix domain socket。

## 传输与安全

| 平台 | 默认 endpoint | 访问控制 |
|---|---|---|
| Windows | `\\.\pipe\com0comd-v1` | DACL 仅 SYSTEM 与 Administrators |
| Linux root | `/run/com0com/control.sock` | socket 文件 `0600` + peer creds 校验 |
| Linux 普通用户 | `$XDG_RUNTIME_DIR/com0com/control.sock` 或 `~/.local/state/com0com/control.sock` | 同用户私有目录 `0700` |
| macOS root | `/var/run/com0com/control.sock` | socket 文件 `0600` + kext 不可用，使用 peer creds |
| macOS 普通用户 | `~/Library/Application Support/com0com/control.sock` | 同用户私有目录 `0700` |

- `COM0COM_ENDPOINT` 覆盖默认 endpoint；`--pipe <name>`（Unix 下等价于 socket 路径）
  再覆盖环境变量。裸名字在 Windows 上会自动补成 `\\.\pipe\<name>`。
- 普通用户客户端在 root daemon 的系统 socket 可访问时优先连它（`0660` + peer creds
  决定是否放行；目录还需对该组可遍历），否则回退到用户目录。
- 每个连接的第一帧必须是 `rpc.hello`，协议版本不匹配会断开。
- 帧格式：一行一个 JSON 对象，以 `\n` 结束；单帧上限 1 MiB。
- daemon 只允许打开后端管理的端口（`serial.open` 不接受任意设备路径）。

## 启动/停止

```powershell
com0comd run --restore --autosave --log C:\ProgramData\com0com\com0comd.log
com0comd status
com0comd stop
com0comd selftest          # 不接触驱动，自检传输与分发
```

| 选项 | 说明 |
|---|---|
| `--pipe <name>` | 覆盖 endpoint |
| `--restore` | 启动时把状态文件恢复到后端 |
| `--autosave` | 变更后防抖保存，退出时最终保存 |
| `--log <path>` | 追加 daemon 日志 |

状态文件默认路径（文件 `0600`，非 root 的父目录 `0700`）：

- Windows：`%ProgramData%\com0com\state.json`
- Linux root：`/var/lib/com0com/state.json`
- Linux 普通用户：`$XDG_STATE_HOME/com0com/state.json` 或 `~/.local/state/com0com/state.json`
- macOS root：`/Library/Application Support/com0com/state.json`
- macOS 普通用户：`~/Library/Application Support/com0com/state.json`

`COM0COM_STATE` 可覆盖路径（测试用）。

Unix 下提供示例服务单元：`packaging/linux/com0comd.service`、
`packaging/macos/org.com0com.daemon.plist`。

## Rust 客户端

```rust
let sdk = com0com_sdk::Sdk::connect_daemon(None)?;
let pairs = sdk.list_pairs()?;
```

`Sdk` 会自动完成握手；断线后所有调用返回 `SdkError::DaemonUnavailable`。

## 请求/响应

```json
{"jsonrpc":"2.0","id":1,"method":"pairs.list","params":{}}
{"jsonrpc":"2.0","id":1,"result":{"pairs":[]}}
```

错误：

```json
{"jsonrpc":"2.0","id":1,"error":{"code":-32004,"message":"pair not found","data":{"kind":"NotFound"}}}
```

## 方法

| 方法 | 参数 | 结果 |
|---|---|---|
| `rpc.hello` | `{"protocol":1,"client":"..."}` | `{protocol,server,version,capabilities,backend}` |
| `rpc.ping` | `{}` | `{pong:true,protocol}` |
| `rpc.shutdown` | `{}` | `{stopping:true}` |
| `driver.info` | `{"kind":"bus"\|"port"}` | `{info}` |
| `driver.logs` | `{"limit":n}` | `{entries}` |
| `driver.capabilities` | `{}` | `{capabilities}` |
| `pairs.list` | `{}` | `{pairs}` |
| `pairs.get` | `{"id":n}` | `{pair}` |
| `pairs.create` | `{"id":n?,"params":{...}}` | `{pair}` |
| `pairs.delete` | `{"id":n}` | `{deleted:n}` |
| `pairs.set_params` | `{"id":n,"params":{...}}` | `{pair}` |
| `pairs.reset_params` | `{"id":n}` | `{pair}` |
| `ports.list` | `{}` | `{ports}` |
| `events.subscribe` | `{"filter":{...}}` | `{subscription}` |
| `events.unsubscribe` | `{"subscription":n}` | `{unsubscribed:n}` |
| `serial.open` | `{"port":"COM15","config":{...}}` | `{handle}` |
| `serial.write` | `{"handle":n,"data":"base64"}` | `{written}` |
| `serial.close` | `{"handle":n}` | `{closed:n}` |
| `state.snapshot` | `{}` | `{snapshot}` |
| `state.save` | `{}` | `{snapshot}` |
| `state.load` | `{}` | `{snapshot}` |
| `state.export` | `{}` | `{json}` |
| `state.import` | `{"json":"...","options":{...}}` | `{report}` |
| `state.restore` | `{"snapshot":{...},"options":{...}}` | `{report}` |
| `daemon.status` | `{}` | `{running,pid,version,protocol,connections,uptime_ms,state_path}` |
| `daemon.logs` | `{"limit":n}` | `{entries}` |

`PairParams`、`PortConfig`、`Snapshot` 的字段与 Rust SDK 一致（serde 默认命名）。

`serial.open` 的 `port` 可以是 `ports.list` 返回的 `name`（Windows 为 `COMx`，
Unix 为链接路径），也可以是 `pairs.get` 返回的 pair 标签（如 `A0`）；其他设备
路径一律返回 `permission` 错误。

## 通知

服务端主动推送，无 `id`：

| 方法 | 参数 |
|---|---|
| `event` | `{event}` |
| `serial.data` | `{handle,data(base64)}` |
| `serial.closed` | `{handle}` |

订阅只对当前连接有效；连接断开后自动清理。

## 错误码

| code | 含义 |
|---|---|
| `-32000` | I/O 错误 |
| `-32001` | 协议错误 |
| `-32002` | 权限不足 |
| `-32003` | 后端不支持 |
| `-32004` | 未找到 |
| `-32005` | 已存在 |
| `-32006` | daemon 不可用 |
| `-32007` | 忙 |
| `-32008` | 驱动不可用 |
| `-32009` | 超时 |
| `-32600/-32601/-32602` | 无效请求 / 未知方法 / 参数错误 |
| `-32603` | 内部错误 |

`data.kind` 给出 `SdkError` 变体；Win32 错误额外带 `data.win32`。

## 最小示例

```json
{"jsonrpc":"2.0","id":1,"method":"rpc.hello","params":{"protocol":1}}
{"jsonrpc":"2.0","id":2,"method":"pairs.create","params":{"id":5,"params":{"port_a":"COM20","port_b":"COM21"}}}
{"jsonrpc":"2.0","id":3,"method":"serial.open","params":{"port":"COM20","config":{}}}
{"jsonrpc":"2.0","id":4,"method":"serial.write","params":{"handle":1,"data":"aGVsbG8="}}
{"jsonrpc":"2.0","method":"serial.data","params":{"handle":1,"data":"aGVsbG8="}}
{"jsonrpc":"2.0","id":5,"method":"serial.close","params":{"handle":1}}
```
