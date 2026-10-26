# C ABI

`com0com-capi` 把 Rust SDK 暴露成稳定的 C ABI，适合 C/C++、C#（P/Invoke）、
Python（ctypes）等调用方。ABI 版本为 `C0C_API_VERSION = 1`。

## 产物

| 平台 | 动态库 | 静态库 |
|---|---|---|
| Windows (MSVC) | `com0com_capi.dll`（可另带 `.dll.lib` 导入库） | `com0com_capi.lib` |
| Linux | `libcom0com_capi.so` | `libcom0com_capi.a` |
| macOS | `libcom0com_capi.dylib` | `libcom0com_capi.a` |

```bash
cargo build -p com0com-capi --release
```

头文件是**生成物**，不要手改：

```bash
cargo run -p com0com-header-gen -- --write crates/com0com-capi/include/com0com_capi.h
cargo run -p com0com-header-gen -- --check crates/com0com-capi/include/com0com_capi.h
```

C 调用方：

```c
#include "com0com_capi.h"

#pragma comment(lib, "com0com_capi")   /* MSVC；其他编译器用 -lcom0com_capi */
```

窗口/静态链接时定义 `C0C_STATIC`，头文件就不会把函数声明成 `dllimport`。
头文件自带 `_Static_assert`/`static_assert` 检查结构体大小，常量
`C0C_SIZEOF_*` 与 Rust 的 `size_of` 由布局测试共同保证。

## 内存与线程规则

- 所有结构体都是 `#[repr(C)]` POD；字符串是定长、NUL 结尾的 `char` 数组。
- 库从不返回需要 `free` 的指针。列表接口使用 `(out, cap, *count)`：
  先用 `out = NULL, cap = 0` 查询数量；返回 `C0C_E_BUFFER_TOO_SMALL` 时
  `*count` 给出所需条目数，缓冲区会尽量填入可容纳的部分。
- `c0c_session`、`c0c_subscription`、`c0c_monitor` 都是不透明句柄。
  同一个 `c0c_session` 可从多线程并发调用（内部加锁）；句柄本身由调用方保证
  在最后一个使用者退出前不被 `c0c_close`/`c0c_unsubscribe`/`c0c_monitor_close`。
- 关闭顺序：先关 monitor 和 subscription，再 `c0c_close(session)`。
- 事件回调在库内部线程执行；`c0c_event.params` 只在回调期间有效，
  需要保留时请拷贝。
- 所有入口都包了 `catch_unwind`；Rust panic 返回 `C0C_E_PANIC`，不会跨 ABI 展开。

## 快速开始

```c
#include "com0com_capi.h"
#include <stdio.h>

int main(void) {
    c0c_session *session = NULL;
    c0c_open_options options = { sizeof(options), C0C_BACKEND_AUTO, NULL };
    int rc = c0c_open(&options, &session);
    if (rc != C0C_OK) {
        c0c_error error;
        c0c_session_last_error(session, &error);
        fprintf(stderr, "open failed: %d %s\n", rc, error.message);
        return 1;
    }

    c0c_params params = {0};
    snprintf(params.port_a, C0C_PORT_NAME_MAX, "%s", "COM15");
    snprintf(params.port_b, C0C_PORT_NAME_MAX, "%s", "COM16");

    c0c_pair_info pair;
    rc = c0c_create_pair(session, C0C_PAIR_ID_AUTO, &params, &pair);
    if (rc == C0C_OK) {
        printf("created pair %u\n", pair.id);
        c0c_remove_pair(session, pair.id);
    }

    c0c_close(session);
    return rc == C0C_OK ? 0 : 1;
}
```

## 打开后端

```c
c0c_open_options options = {
    .size = sizeof(c0c_open_options),
    .backend = C0C_BACKEND_DAEMON,
    .endpoint = "\\\\.\\pipe\\com0comd-v1",  /* NULL = 平台默认 */
};
c0c_open(&options, &session);
```

`backend` 取值：`C0C_BACKEND_AUTO`（等同 `NATIVE`）、`C0C_BACKEND_NATIVE`、
`C0C_BACKEND_DAEMON`、`C0C_BACKEND_FAKE`（仅带 `fake` feature 构建的测试库）。

`options.size` 必须不小于库认识的结构体大小，便于以后扩展字段。

## 串口对与参数

```c
c0c_pair_info pairs[64];
size_t count = 0;
c0c_list_pairs(session, pairs, 64, &count);

c0c_params params;
c0c_get_params(session, 5, &params);
params.baud_emu = 1;
params.noise_enabled = 1;
params.noise_probability = 2500000;      /* 2.5%（分母 1e8） */
params.all_data_bits = 1;                /* 保留 8 位，不按字长截断 */
c0c_set_params(session, 5, &params);

c0c_port_info ports[64];
c0c_list_ports(session, ports, 64, &count);
```

`noise_probability` 超出 `0..=100000000` 返回 `C0C_E_INVALID_ARGUMENT`。
`c0c_params` / `c0c_pair_info` 末尾新增 `all_data_bits`，对应大小常量
（`C0C_SIZEOF_C0C_PARAMS` / `C0C_SIZEOF_C0C_PAIR_INFO`）已同步更新。

`pin_wiring_a` / `pin_wiring_b` 的四个字节依次是本端口 RTS/DTR/OUT1/OUT2
驱动到对端的输入线位（CTS=0x10、DSR=0x20、RI=0x40、DCD=0x80）。**全零视为
"未指定"**，SDK 按上游默认空 Modem 接线填充（CTS←RTS、DSR 与 DCD←DTR、
OUT1/OUT2 悬空）；`c0c_params p = {0}` 这种清零写法因此仍能正常握手。

## 事件

```c
static void on_event(void *user, const c0c_event *event) {
    if (event->kind == C0C_EVENT_PAIR_ADDED && event->params)
        printf("pair %u added\n", event->pair_id);
}

c0c_subscription *sub = NULL;
c0c_subscribe(session, on_event, NULL, &sub);
/* ... */
c0c_unsubscribe(sub);
```

`event.kind`：`C0C_EVENT_PAIR_ADDED`、`PAIR_REMOVED`、`PARAMS_CHANGED`、
`DRIVER_STATUS_CHANGED`（后者用 `driver_kind`/`driver_status`，`pair_id` 为
`C0C_NO_PAIR`）。

## 监视器

```c
c0c_port_config config = { .baud_rate = 115200 };
c0c_monitor *monitor = NULL;
c0c_open_monitor(session, "COM15", &config, &monitor);

size_t written = 0;
c0c_monitor_write(monitor, data, len, &written);

uint8_t buffer[1024];
size_t read = 0;
c0c_monitor_read(monitor, buffer, sizeof(buffer), 500 /* ms */, &read);
/* C0C_TIMEOUT_NONE = 无限等待 */

uint64_t dropped = c0c_monitor_dropped(monitor);
c0c_monitor_close(monitor);
```

`c0c_monitor_read` 超时返回 `C0C_E_TIMEOUT`（`*read = 0`）。队列有界，
读得慢会丢块并累加 `c0c_monitor_dropped`。

## 状态

```c
size_t needed = 0;
c0c_state_export_json(session, NULL, 0, &needed);   /* C0C_E_BUFFER_TOO_SMALL */
char *json = malloc(needed);
c0c_state_export_json(session, json, needed, &needed);

c0c_restore_options options = { .dry_run = 0, .remove_extra = 0, .continue_on_error = 1 };
c0c_restore_report report;
c0c_state_import_json(session, json, &options, &report);
printf("applied=%u failed=%u\n", report.applied, report.failed);
```

schema 固定为 1；不匹配返回 `C0C_E_PROTOCOL`。

## 错误

每个入口返回 `int32_t`：`C0C_OK` 或负错误码（见 `com0com_capi.h`）。

```c
c0c_error error;
if (c0c_session_last_error(session, &error) == C0C_OK)
    fprintf(stderr, "code=%d win32=%u %s\n", error.code, error.win32, error.message);

char message[C0C_MESSAGE_MAX];
c0c_session_last_error_message(session, message, sizeof(message));
```

`*_last_error` 记录该 session 上最近一次失败的调用；成功调用会清空它。

## 诊断

```c
c0c_driver_desc info;
c0c_driver_info(session, C0C_DRIVER_BUS, &info);
printf("%s %d %s %s\n", info.service_name, info.status, info.version, info.sha256);

c0c_log_entry entries[32];
size_t count = 0;
c0c_driver_logs(session, 32, entries, 32, &count);
```

Windows 上读取 SCM 状态、文件版本与 SHA-256，以及 `System` 事件通道。
PTY/其他后端返回 `C0C_E_UNSUPPORTED`。

## 测试

```bash
cargo run -p com0com-capi-smoke -- --self-test   # 内存后端，无需驱动
cargo run -p com0com-capi-smoke -- --native      # 真实驱动
```

`--self-test` 覆盖布局、CRUD、参数、事件、monitor、状态导入导出与错误码，
输出 `PASS|`/`FAIL|` 与 `RESULT| capi-smoke|N/N|PASS`。
