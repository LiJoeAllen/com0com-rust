# com0com-rust

用 Rust 对 **com0com**（Windows 虚拟 null-modem 串口驱动）进行的一次**全量重写**。

目标不是“能加载的雏形”，而是一个真正可用的虚拟串口对仿真器：

- 不限数量的动态串口对（`setupc` 创建/删除）；
- 完整的 `IOCTL_SERIAL_*` 表面，使 `CreateFile("COMx")` 与整套 Win32 Comm API 透明可用；
- 两端口之间通过共享内存环缓冲做数据转发，并实现 modem 信号映射；
- 用户态配置工具 `setupc`（对标原版 `setupc`/`setupg`）。

> ⚠️ **状态**：核心架构、两套驱动源码、共享协议、`setupc` 均已完成。
> 两套内核驱动已在 **WDK 10.0.28000 + wdk-sys 0.4.0** 环境下编译通过，并已于
> 2026-10-21 在 **Windows 11 24H2 测试虚拟机**中完成加载与端到端验证：
> `setupc add/remove/list`、COM15/COM16 双向 4096 字节传输、自动命名，以及删除后
> 端口/PHANTOM/`SERIALCOMM` 清理均通过。验证流程已固化在
> [`tools/vm-test/`](tools/vm-test/README.md)。
> 没有 WDK 的机器仍只能构建 `com0com-common` 与 `setupc` 两个纯用户态/无 std 的包。

---

## 架构（混合驱动模型）

采用**混合模型**：

| 组件 | 模型 | 产物 | 说明 |
|------|------|------|------|
| `com0combus`  | **KMDF** 总线驱动 | `com0combus.sys` | 创建根 FDO，枚举每对串口的 2 个 PDO；持有每对共享的 `PairContext`（非分页池），通过私有 IOCTL 暴露给端口驱动与 `setupc`。 |
| `com0comport` | **WDM** 端口功能驱动 | `com0comport.sys` | 运行在总线枚举出的 PDO 上；实现 `IOCTL_SERIAL_*`，把数据在成对端口间转发；通过 `IOCTL_COM0COM_PORT_LINK` 拿到 `PairContext` 的内核虚地址（同地址空间，裸指针共享合法）。 |
| `setupc`      | 用户态 Win32 控制台 | `setupc.exe`      | 通过 `GUID_COM0COM_BUS` 设备接口打开总线，发出私有 `IOCTL_COM0COM_*` 来增删查串口对与参数。 |
| `com0com-common` | 无 std 共享库 | — | `IOCTL_*` 控制码、`SERIAL_*` 结构体、`PairContext`/环缓冲、私有协议结构。内核态与用户态共同依赖，保证线格式一致。 |

**关键设计点**

- 总线为每对分配一个 `PairContext`（含两个 `PortState` 与一把 `KSPIN_LOCK`），放在非分页池；端口驱动通过 `PORT_LINK` 取得内核 VA 并引用计数，两块驱动在同一内核地址空间，裸指针共享有效。
- 串口对由**兼容 ID** 编码：`c0c0_<pair_id_hex8><A|B>`（如 `c0c0_00000005A`），端口驱动据此恢复 `pair_id`/`port_index`，而不影响 INF 的硬件 ID 匹配。
- COM 命名：`\DosDevices\COMx` 符号链接 + `GUID_DEVINTERFACE_COMPORT` 设备接口注册，Win32 应用可直接 `CreateFile("COMx")`。
- 跨驱动共享状态、取消安全的挂起 IRP（read / `WAIT_ON_MASK`）均由 `PairContext` 的自旋锁保护。

---

## 目录结构

```
com0com-rust/
├── Cargo.toml                 # 工作区；default-members 仅含可在本机构建的包
├── crates/
│   ├── com0com-common/        # 共享（no_std）数据结构/IOCTL/协议
│   ├── com0combus/            # KMDF 总线驱动（com0combus.sys）
│   │   ├── src/{bus,driver,pdo,ioctl}.rs
│   │   ├── com0combus.inx     # 总线 INF 模板
│   │   └── Makefile.toml      # cargo-wdk 构建脚本
│   └── com0comport/           # WDM 端口驱动（com0comport.sys）
│       ├── src/{driver,dispatch,readwrite,serial,comname}.rs
│       ├── com0comport.inx    # 端口 INF 模板
│       └── Makefile.toml
└── tools/
    ├── setupc/                # 用户态配置工具（com0com-setupc）
    │   ├── Cargo.toml
    │   └── src/main.rs
    └── vm-test/               # 隔离 VM 的构建/安装/端到端验证脚本
        ├── README.md
        ├── build-package.ps1
        ├── run-vm-test.ps1
        └── guest/             # 在客户机内执行的安装与检查脚本
```

> 工作区 `default-members` 被设为 `com0com-common` 与 `tools/setupc`，因此一条裸 `cargo build`
> 只构建这两个可在无 WDK 机器上编译的包，避免去触碰需要 WDK 的内核 crate。
>
> 两个内核驱动**不是**本工作区的成员，各自是独立的工作区根（见下文“构建”），
> 因为它们声明的驱动模型不同（KMDF vs WDM），而 `wdk-build` 要求整张依赖图只有一种配置。

---

## 环境（在**构建机 / 测试虚拟机**上准备）

内核驱动无法用普通 `cargo build` 产生——它需要 **WDK（Windows Driver Kit）** 提供的
头文件、库与代码签名链。当前 `windows-drivers-rs` 生态（`wdk` / `wdk-sys` /
`wdk-build` / `wdk-alloc` / `wdk-panic` / `wdk-macros`）是事实标准，请按以下步骤准备：

1. **安装 WDK / eWDK 与对应 Visual Studio + Windows SDK。**
   - 推荐用微软的 **eWDK（Enterprise WDK）** 镜像，自带匹配的 SDK 与工具链。
   - WDK 自带 Clang 用于编译内核（而非 MSVC），`cargo-wdk` 会调用它。
   - **bindgen 使用的 libclang 请选 18–20（重要）**：libclang **22.1.x** 存在
     `clang_getTypeDeclaration` 回归——对 `typedef struct X X; struct X {...};` 返回前向声明
     而非定义，导致 `wdk-sys` 生成的结构体退化为 `_address: u8`，编译时报几十条
     `E0080 size_of 溢出`（很容易误判成“没装 WDK”）。用 `LIBCLANG_PATH` 指向 LLVM 18 的
     `bin` 目录即可；也可 `py -m pip install libclang==18.1.1` 后指向 wheel 内的 `clang/native`。
   - **WDK 10.0.28000 的 include 目录不含 SDK 头文件**：`Include/10.0.28000.0` 下只有
     `km`/`shared` 等 WDK 负载，`shared`/`ucrt`/`um` 需由另一版本的 Windows SDK
     （如 10.0.26100.0）提供。bindgen 报 `specstrings.h` 之类缺失时，用
     `BINDGEN_EXTRA_CLANG_ARGS` 追加 SDK 的 include 路径；该变量按空白切分，路径不能含空格
     （可先做 junction/symlink 到无空格路径）。

2. **安装 Rust 工具链与 `cargo-wdk`：**
   ```powershell
   rustup toolchain install stable
   cargo install cargo-wdk
   ```
   `cargo-wdk` 会在构建时调用 WDK 的 `wdk-build` 生成对应驱动模型（KMDF / WDM）的绑定。

3. **关于 WDM 绑定的注意点（重要）**
   - crates.io 上的 `wdk-sys` 仅附带一份**预生成的 KMDF v1.33** 绑定作为“无 WDK 机器”的兜底。
   - 端口驱动是 **WDM**。要让 `wdk-sys` 生成 WDM 绑定，必须在**已安装 WDK** 的环境下构建：
     `com0comport/Cargo.toml` 中已声明
     `[package.metadata.wdk.driver-model] driver-type = "WDM"`，
     `cargo wdk build` 会据此用 WDK 头文件重新生成正确的 WDM 绑定。
   - 因此：在没有 WDK 的机器上**不要**尝试 `cargo wdk build -p com0comport`，它只会拿到 KMDF 兜底绑定而失败；请在 eWDK 环境构建。

4. **测试虚拟机（用于实际加载驱动）**
   - 开启测试签名：`bcdedit /set testsigning on` 后重启。
   - 或配置测试证书并对 `.sys`/`.cat` 做测试签名（见微软“Signing a Driver for Test”文档）。

---

## 构建

```powershell
# 用户态工具（无需 WDK，普通 cargo 即可；目标 x86_64-pc-windows-msvc）
cargo build -p com0com-setupc --release

# 内核驱动（需要 WDK/eWDK；各自是独立工作区根，必须用 --manifest-path 指定）
cargo build --manifest-path crates/com0combus/Cargo.toml --release
cargo build --manifest-path crates/com0comport/Cargo.toml --release
```

> 本机实测通过的构建环境（WDK 10.0.28000 + Windows SDK 10.0.26100 + libclang 18.1），
> 需要显式指定 libclang 与 SDK 头文件路径（原因见上文“环境”）：
> ```powershell
> $env:LIBCLANG_PATH = "D:\path\to\llvm18\bin"
> $env:BINDGEN_EXTRA_CLANG_ARGS = "-I D:/path/to/sdk26100/shared -I D:/path/to/sdk26100/ucrt -I D:/path/to/sdk26100/um"
> cargo build -j 2 --manifest-path crates/com0combus/Cargo.toml
> ```
> 首次构建会为 `wdk-sys` 跑 bindgen，耗时且吃 CPU；弱机器务必限制并行度（`-j 2`）。

> **为什么不能用 `cargo build -p com0combus`**：`wdk-build` 会遍历整张 cargo-metadata
> 依赖图并要求只有一种 WDK 驱动模型配置。`com0combus` 声明 KMDF 1.33、`com0comport`
> 声明 WDM，二者同处一个工作区时**任何**构建都会在编译开始前就中止：
> `multiple configurations of the WDK are detected ... { Kmdf(1,33), Wdm }`。
> 拆成独立工作区后各自依赖图单一配置；顺带它们 Cargo.toml 里的 `[profile.*] panic = "abort"`
> 也真正生效了（作为工作区根不会再被忽略而只报告警）。

> 内核 crate 的编译与最终 `.sys` 必须在装有 **WDK** 的环境完成。仅装 Windows SDK 不够：
> 缺 `Windows Kits\10\Include\<ver>\km`（内核头文件）时，`wdk-sys` 由 bindgen 生成的大量结构体
> 会退化成不完整类型（size 为 1），进而爆出几十条 `E0080 size_of 溢出` 断言失败。

---

## 安装与在测试 VM 中运行

推荐使用已固化的自动化流程（构建 → 快照回滚 → 安装 → 双向传输验证 → 收集日志）：

```powershell
powershell -ExecutionPolicy Bypass -File tools\vm-test\build-package.ps1
powershell -ExecutionPolicy Bypass -File tools\vm-test\run-vm-test.ps1 `
  -VmxPath 'F:\VMs\com0com-test\com0com-test.vmx' `
  -GuestUser '<客户机用户名>' -GuestPassword '<客户机密码>' `
  -SnapshotName clean
```

前置条件、参数与排查见 [`tools/vm-test/README.md`](tools/vm-test/README.md)。
手工安装步骤如下。

1. 把 `.inx` 模板交给 WDK 处理成 `.inf`（WDK 的 `stampinf` / MSBuild 会写入版本与日期），
   或直接按模板生成：
   ```powershell
   # 在 eWDK 构建后，通常已生成 com0combus.inf / com0comport.inf
   ```

2. 安装总线（根枚举设备），再让总线枚举出端口 PDO 时自动匹配端口 INF：
   ```powershell
   # 以管理员身份
   devcon install com0combus.inf COM0COM\BUS
   # 端口 INF 由系统在总线枚举 PDO（COM0COM\PORT_A/B）时自动匹配安装
   ```
   也可用 `pnputil /add-driver com0combus.inf /install`。

3. 用 `setupc` 创建一对虚拟串口：
   ```powershell
   setupc add --port-a COM15 --port-b COM16
   setupc list
   ```
   此时 `COM15` 与 `COM16` 即为一个 null-modem 对，可用任意串口工具互发数据。

4. 删除：`setupc remove 0`。

---

## `setupc` 用法

```
setupc list
setupc add   [--id N] [--port-a NAME] [--port-b NAME]
             [--baud-emu] [--overrun] [--noise[=PROB]] [--exclusive] [--plugin]
setupc remove <id>
setupc params <id>
setupc set   <id> [same flags as add, minus --id/--port-*]
```

- 省略 `--port-a/--port-b` 时由系统 COM 名称仲裁分配 `COMx`。
- `--noise=PROB` 中的 `PROB` 为 0..10000（单位 0.01%，即 `PROB/100` 为百分比）。
- 省略 `--id` 时自动选择最小可用 id（上限 64 对）。
- 需要**管理员权限**运行（打开总线设备接口需要写访问）。
- 排查问题时设置环境变量 `COM0COM_TRACE=1`，`setupc` 会把设备路径与每次 IOCTL 调用
  以 `[setupc]` 前缀写到 stderr，不影响正常输出。

`setupc` 的实现位置：`tools/setupc/src/main.rs`。它通过
`GUID_COM0COM_BUS`（`com0com-common` 中的 `GUID_COM0COM_BUS_BYTES`）枚举总线设备接口，
再用 `CreateFileW` 打开并发出 `IOCTL_COM0COM_CREATE_PAIR` / `DELETE_PAIR` / `ENUM_PAIRS` /
`GET_PARAMS` / `SET_PARAMS`。

---

## 已实现 vs. 待办（P2 特性）

**已实现**
- 混合驱动骨架、PDO 枚举、兼容 ID、`PORT_LINK`/`PORT_UNLINK` 引用计数与回收；
- 完整 `IOCTL_SERIAL_*`（波特率/线路控制/字符/超时/握手/modem 状态/等待事件/统计/中断/流控等）；
- 两端口数据转发、挂起读与 `WAIT_ON_MASK` 的取消安全实现；
- `setupc` 用户态配置工具；共享协议与结构体；
- `emu_br`（波特率时序仿真）：按 `baud` + 帧格式（半比特精确计数，支持 1.5 停止位）给写路径发放字节预算；
  **实现方式**：写侧短写限速（`Information` = 本次实际投递字节数），**无 staging 缓冲、无定时器/DPC**；
  读侧不限速。已知边界：调用方需重试短写；高频小写入靠 Q16 定点 carry 累积，不会饿死。
- `emu_noise`（噪声注入）：按 `noise_probability`（0..10000，单位 0.01%）逐字节翻转一个随机位，
  使用确定性 `Xorshift32`（种子存在 `PortState.rng_state`，可复现）；**实现方式**：256 字节栈上分块
  （不在内核栈开 16KB），破坏计数落到 `noise_count` / `FrameErrorCount` / `ParityErrorCount`。
  已知边界：同一“写”只把帧/奇偶错误计数各 +1，不是逐字节计数。
- `emu_overrun`（溢出仿真）：写侧被对端队列上限截断时，把差额计入对端
  `SerialOverrunErrorCount` / `BufferOverrunErrorCount` 并置 `SERIAL_EV_ERR`；
  未开启该标志时的截断只是正常流控，不计错误。
- `IOCTL_SERIAL_SET_QUEUE_SIZE`：真正生效——`InSize`/`OutSize` 经 `clamp_queue_size` 夹到
  `[1, RING_BUFFER_SIZE-1]`，并写入 `rx_buffer_size`/`tx_buffer_size`/`queue_limit`；
  写路径用 `queue_limit` 作为环缓冲上限（`RingBuffer::write_capped`），`GET_PROPERTIES`
  的 `MaxTxQueue`/`MaxRxQueue`/`Current*Queue` 也随之反映。已知边界：环缓冲物理大小不变，
  缩小队列不会释放内存。
- `IOCTL_SERIAL_XOFF_COUNTER`：真正生效——按 `XoffState` 语义限制本次可投递字节数，
  计数耗尽即 halt，直到 `SET_XON`/`PURGE` 清除；`SET_XOFF` 为立即 halt。
- 挂起 IRP 正确性：返回 `STATUS_PENDING` 前调用 `IoMarkIrpPending`，且在**取消自旋锁**下
  安装 cancel routine 并复查 `Irp->Cancel`（消除“已取消但未回调 → IRP 泄漏”竞态）。

**待实现**
- 基于注册表的持久化配置（对标 `HKLM\...\com0com\Parameters`）；
- 网络扩展（远程串口对）。

---

## 安全性与零拷贝立场

本项目遵循“尽量少 unsafe、尽量零拷贝”的原则，分两层落实：

- **`com0com-common`（共享库）**：100% 安全 Rust，零 `unsafe`、零多余拷贝。所有
  `IOCTL_*` 控制码、协议结构体、`PairContext`/环缓冲都是纯 `const`/数据结构。
- **`setupc`（用户态工具）**：业务命令逻辑（`cmd_*` / `main`）**零 `unsafe` 关键字**。
  `unsafe` 全部收敛到极薄的 Win32 FFI 封装 helper（`bus_device_path` / `open_bus` /
  `ioctl` / `struct_bytes` / `struct_of` / `close_bus` / `from_bytes`）。请求发送采用
  **零拷贝**：直接把 `#[repr(C)]` 结构体以字节切片视图传给 `DeviceIoControl`，
  不再 `memcpy` 进 `Vec`。返回数据的一次拷贝是 `METHOD_BUFFERED` IOCTL 的硬性要求
  （内核写回的是裸字节，且结构体对齐可能高于 `u8` 缓冲），属于必要拷贝，无冗余。
- **`com0combus` / `com0comport`（内核驱动）**：**无法做到零 unsafe**——调用
  WDK/wdk-sys FFI、操作 `KSPIN_LOCK` / `PIRP` / 裸指针、挂起并取消 IRP 本质就是
  `unsafe`。但所有 unsafe 同样收敛到小 helper（`read_in` / `write_out` / `lock` /
  `unlock` / `read_struct` / `write_struct` / `raw_lock` / `raw_unlock` 等），
  数据流上**只有一次必要的边界拷贝**，没有冗余拷贝。内核驱动已在 WDK 环境编译通过，
  并在隔离测试虚拟机中完成加载与端到端验证（见 [`tools/vm-test/`](tools/vm-test/README.md)）。

## 已知风险 / 需在上机验证的点

- **编译已验证**（2026-10-21）：`com0comport`（WDM）与 `com0combus`（KMDF 1.33）均基于
  WDK 10.0.28000 + `wdk-sys` 0.4.0 编译通过。普通 `cargo build` 产出的是 `.dll`
  （`cargo wdk build` 才会生成并改名 `.sys`）。
- **已在测试虚拟机验证**（2026-10-21）：两套驱动在 Windows 11 24H2（testsigning）中加载运行；
  `setupc add/remove/list`、双向数据传输、自动命名、删除后的设备/`SERIALCOMM` 清理均通过。
  复现见 [`tools/vm-test/`](tools/vm-test/README.md)。
- 验证环境为 VMware + Win11 24H2 评估版；KMDF 1.33 要求 **Win11 24H2 / Server 2025 或更高**。
- 配置持久化尚未实现：重启后串口对不会保留，需要重新 `setupc add`。
- 内核 crate `Cargo.toml` 内含 `[profile.dev]/[profile.release] panic = "abort"`
  （内核不支持 unwind）。二者现已是独立工作区根，该 profile **会正常生效**，
  不再出现“profile 被忽略”的告警。
- 真实加载前，务必在**隔离的测试虚拟机**中开启测试签名，避免影响宿主机。

---

## 许可

MIT OR Apache-2.0（与 `windows-drivers-rs` 生态一致）。
