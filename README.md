# com0com-rust

用 Rust 重写的 **com0com** 虚拟 null-modem 串口对：Windows 保留 Rust 内核驱动，
Linux/macOS 使用纯 Rust 用户态 PTY，两者共用同一套分层 SDK、C ABI 与本机 JSON-RPC
守护进程。仓库**不包含任何手写 C/C++ 源文件**，也不提供具体 GUI——只提供让 UI
像调用 SDK 一样使用的契约与实现。

- Windows：Rust 内核驱动（`com0combus` KMDF 总线 + `com0comport` WDM 端口），
  `CreateFile("COMx")` 与整套 Win32 Comm API 透明可用；
- Linux/macOS：无内核模块，PTY 对由 `com0comd` 持有，稳定链接暴露为普通设备路径；
- 三种 SDK 形态：Rust 门面 `com0com-sdk`、C ABI `com0com_capi` + 生成头文件、
  本机 JSON-RPC（`com0comd`，无网络监听）；
- 能力位诚实反映实现差异：Unix 不具备的内核级能力一律为 `false`，不支持参数返回
  `Unsupported`，不会静默伪造。

> ⚠️ **状态**：Phase 0–7 的分层架构、Windows 内核驱动、Unix PTY 后端、SDK/IPC/
> daemon/C ABI 均已实现。Windows 侧于 2026-10-24 在 **Windows 11 24H2 测试虚拟机**
> 完成 47 项断言（`setupc` 31 项 + SDK/C ABI/RPC、波特率仿真写路径与
> daemon `--restore`/`--autosave`），
> 驱动 SHA-256 与构建产物一致；Unix PTY 已在 Linux（WSL2，51/51）实机通过，
> macOS 仅完成交叉编译检查，端到端脚本为
> [`tools/vm-test/guest/check-pairs-unix.sh`](tools/vm-test/guest/check-pairs-unix.sh)。
> 平台差异、能力矩阵与部署步骤见 [`docs/platforms.md`](docs/platforms.md)。

---

## 分层架构

```
UI / 业务代码
   │  Rust facade            C ABI                    JSON-RPC
   ▼                         ▼                        ▼
com0com-sdk ─────────── com0com-capi ─────────── com0com-ipc ── com0com-daemon
   │                         │                        (named pipe / Unix socket)
   ▼                         ▼
com0com-core（模型、能力位、错误、事件、Backend/Monitor trait、状态快照）
   │
   ├── com0com-backend-win   Windows：SetupAPI + DeviceIoControl
   ├── com0com-backend-pty   Linux/macOS：纯 Rust PTY + 用户态仿真
   └── com0com-backend-stub  其他平台：全部 Unsupported
```

**关键设计点**

- 所有平台共享 `com0com-core` 的 `PairId`/`PairParams`/`PortInfo`/`Event`/`SdkError`
  与状态 schema，UI 不需要按平台写业务分支，只查能力位。
- `com0com-sdk` 在 Unix 上自动探测本机 daemon，失败才退回进程内 PTY；
  Windows 默认同进程 IOCTL，设置 `COM0COM_DAEMON=1` 时改走 named pipe。
- `com0comd` 只监听本机 IPC（无 TCP/UDP），Unix socket 默认 `0600`，可用
  `--allow-uid` 切换为 `0660` 并逐连接校验 peer uid；`serial.open` 只接受后端管理的端口。
- 状态文件采用临时文件 + `sync_all` + rename 原子写入；`--restore` 在启动时恢复，
  `--autosave` 在变更后防抖保存并在退出时最终保存。

## 驱动模型（Windows，保持不变）

| 组件 | 模型 | 产物 | 说明 |
|------|------|------|------|
| `com0combus`  | **KMDF** 总线驱动 | `com0combus.sys` | 创建根 FDO，枚举每对串口的 2 个 PDO；持有每对共享的 `PairContext`（非分页池），通过私有 IOCTL 暴露给端口驱动与 `setupc`。 |
| `com0comport` | **WDM** 端口功能驱动 | `com0comport.sys` | 运行在总线枚举出的 PDO 上；实现 `IOCTL_SERIAL_*`，把数据在成对端口间转发；通过 `IOCTL_COM0COM_PORT_LINK` 拿到 `PairContext` 的内核虚地址（同地址空间，裸指针共享合法）。 |
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
├── Cargo.toml                 # 根 workspace；三个 workspace 根见下文
├── crates/
│   ├── com0com-common/        # 共享（no_std）数据结构/IOCTL/协议
│   ├── com0com-core/          # 领域模型、能力位、Backend/Monitor trait、状态
│   ├── com0com-backend-win/   # Windows SetupAPI + IOCTL 后端
│   ├── com0com-backend-pty/   # Linux/macOS 纯 Rust PTY 后端
│   ├── com0com-backend-stub/  # 未知平台 Unsupported 后端
│   ├── com0com-sdk/           # Rust 门面（事件、Monitor、状态、daemon 选择）
│   ├── com0com-ipc/           # JSON-RPC DTO/分帧/传输/RemoteBackend
│   ├── com0com-daemon/        # com0comd 二进制与 serve 核心
│   ├── com0com-capi/          # C ABI 动态库 + 生成头文件
│   ├── com0combus/            # KMDF 总线驱动（独立 workspace 根）
│   └── com0comport/           # WDM 端口驱动（独立 workspace 根）
├── docs/                      # sdk / jsonrpc / c-abi / platforms
├── packaging/                 # systemd 与 launchd 示例
└── tools/
    ├── check-deps.ps1         # 第三方依赖版本一致性检查 / -Fix 同步
    ├── setupc/                # 跨平台 CLI（com0com-setupc）
    ├── sdk-smoke/             # SDK 冒烟（fake/native/daemon）
    ├── capi-smoke/            # C ABI 冒烟与布局自检
    ├── header-gen/            # 从 Rust 定义生成 C 头文件快照
    └── vm-test/               # 隔离 VM 与 Unix PTY 端到端脚本
```

> 根 workspace 的 `default-members` 只含宿主可构建的包；两个内核驱动各自是独立
> workspace 根，因为 KMDF 与 WDM 驱动模型不能共存于同一张依赖图（`wdk-build` 会拒绝）。
>
> 第三方依赖版本只在根 `Cargo.toml` 的 `[workspace.dependencies]` 中维护。两个驱动根
> 无法继承该表，只能重复声明；`tools/check-deps.ps1` 会在 pre-commit 与 CI 中强制三者
> 完全一致，并禁止包内依赖直接写版本号。`-Fix` 可一键同步。
>
> `.cargo/config.toml` 的 `crt-static` 只作用于 Windows 目标，Linux/macOS 构建
> （尤其是 C ABI 动态库）不受影响。

---

## 环境清单（`.env`，新机器先看这里）

仓库根的 `.env` 是团队共享模板，列出了构建与虚拟机测试所需的**全部**可配置项。
clone 之后只需两步：

```powershell
cp .env .env.local        # 1. 复制一份本机配置
# 2. 编辑 .env.local：填自己的 WDK 路径、虚拟机 .vmx、客户机账号口令等
```

`tools/vm-test/` 下的脚本会**自动加载** `.env` / `.env.local`，填完即可直接运行；
直接执行 `cargo` 命令时再手动加载：

```powershell
. tools/load-env.ps1      # PowerShell 必须 dot-source
```
```bash
source tools/load-env.sh  # Git Bash 必须 source
```

| 变量 | 用途 |
|------|------|
| `WDKContentRoot` | WDK 内容根目录（须含 `Include\<ver>\km` 与 `Lib\wdf\kmdf`）；留空自动探测 |
| `CARGO_BUILD_JOBS` | cargo 并行度，默认 2——内核 crate 首次 bindgen 极吃 CPU，别调太高 |
| `LIBCLANG_PATH` | **必须是 libclang 18（18.1.1 已验证）**。22.1.x 有 bindgen 回归，会让 `wdk-sys` 编译爆成片 `E0080` |
| `BINDGEN_EXTRA_CLANG_ARGS` | 给 bindgen 补 SDK 的 `shared/ucrt/um` 头文件路径（WDK 自带 Include 里没有）；路径不能含空格 |
| `WDK_BIN_PATH` | 含 `signtool.exe` 的 SDK 版本化 bin 目录；会前置到 `PATH`，避免 MSYS 同名工具抢先命中 |
| `DEVCON_PATH` | `devcon.exe` 完整路径；留空时在 `Windows Kits\10\Tools\**\x64` 自动查找 |
| `COM0COM_TRACE` | `setupc` 诊断开关：非空即开启，日志写 stderr |
| `VMRUN_VMX_PATH` / `VMRUN_GUEST_USER` / `VMRUN_GUEST_PASSWORD` | 虚拟机端到端测试目标（凭据只写 `.env.local`） |
| `VMRUN_PATH` / `VMRUN_SNAPSHOT_NAME` | `vmrun.exe` 路径（留空自动查找）/ 要回滚的关机快照 |
| `VM_TEST_CONFIGURATION` / `VM_TEST_JOBS` | 构建配置（debug/release）与并行度 |
| `VM_TEST_PACKAGE_DIR` / `VM_TEST_RESULTS_DIR` | 测试包与日志输出目录；留空用 `target/` 下的默认位置 |
| `VM_TEST_GUEST_DIR` / `VM_TEST_BOOT_TIMEOUT_SEC` | 客户机工作目录 / 等待客户机就绪的超时秒数 |

规则：
- **`.env` 入库，只放占位值**，禁止写真实口令；本机私密值一律写 **`.env.local`**（已 gitignore）
- 优先级：命令行显式传参 → `.env.local` → 已有环境变量 → `.env` → 脚本默认值
  （`.env.local` 刻意能压过机器上遗留的环境变量——例如系统装的 libclang 22）
- 核对实际生效值：`bash tools/load-env.sh --print` 或 `pwsh -File tools/load-env.ps1 -Print`
  （口令自动打码）
- 输入路径（`LIBCLANG_PATH`、`WDKContentRoot`、`DEVCON_PATH` 等）的相对值会锚定到仓库根；
  `VM_TEST_*_DIR` 这类输出目录即使尚不存在也按仓库根解析；`VM_TEST_GUEST_DIR` 是客户机路径，原样传递

> **为什么是三个 workspace**：`com0combus`（KMDF）与 `com0comport`（WDM）驱动模型不同，
> 而 `wdk-build` 要求单张依赖图只有一种配置，同处一个 workspace 会让**任何**构建中止。
> 二者因此各自成为独立 workspace 根，它们的依赖版本靠 `tools/check-deps.ps1` 强制与根一致。

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
     `bin` 目录即可；也可 `py -m pip install "libclang==18.1.1" --target target/libclang18`
     后指向 wheel 内的 `clang/native`（`.env` 的默认值就是它）。
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
# 用户态工具与 SDK（无需 WDK）
cargo build -p com0com-setupc -p com0com-daemon -p com0com-sdk -p com0com-capi --release
cargo build -p com0com-sdk-smoke -p com0com-capi-smoke -p com0com-header-gen

# 内核驱动（需要 WDK/eWDK；各自是独立工作区根，必须用 --manifest-path 指定）
cargo build --manifest-path crates/com0combus/Cargo.toml --release
cargo build --manifest-path crates/com0comport/Cargo.toml --release
```

Linux/macOS 上同一条命令即可构建 CLI、daemon 与 SDK（PTY 后端不需要内核头文件）：

```bash
cargo build -p com0com-setupc -p com0com-daemon -p com0com-sdk-smoke
```

> `com0comd`、`setupc`、`sdk-smoke` 与 C ABI 动态库的产物名分别为 `com0comd`、
> `setupc`、`sdk-smoke`、`com0com_capi`。

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

推荐使用已固化的自动化流程（构建 → 快照回滚 → 安装 → 双向传输验证 → 收集日志）。
`.env.local` 填好 `VMRUN_*` 后，一条命令即可：

```powershell
powershell -ExecutionPolicy Bypass -File tools\vm-test\run-vm-test.ps1
```

连接参数仍可用命令行覆盖（优先级高于 `.env.local`）：

```powershell
powershell -ExecutionPolicy Bypass -File tools\vm-test\run-vm-test.ps1 `
  -VmxPath 'F:\VMs\com0com-test\com0com-test.vmx' `
  -GuestUser '<客户机用户名>' -GuestPassword '<客户机密码>' `
  -SnapshotName clean
```

只重建测试包而不跑 VM：`tools\vm-test\build-package.ps1`；复用已有测试包：
`run-vm-test.ps1 -SkipBuild`。

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

Linux / macOS 不需要安装驱动：构建后由 `com0comd`（或进程内 PTY 后端）创建
`/dev/com0com/<label>` 或 `/dev/cu.com0com-<label>` 链路，非 root 用户自动退化为
用户目录下的端口路径。systemd / launchd 单元与完整说明见
[`docs/platforms.md`](docs/platforms.md)。

---

## `setupc` 用法

```
setupc list
setupc add   [--id N] [--port-a NAME] [--port-b NAME]
             [--baud-emu] [--overrun] [--noise[=PROB]] [--all-data-bits]
             [--exclusive] [--plugin]
setupc remove <id>
setupc params <id>
setupc set   <id> [same flags as add, minus --id/--port-*]
```

- 省略 `--port-a/--port-b` 时：Windows 由系统 COM 名称仲裁分配 `COMx`；
  Unix 默认使用 `A{id}` / `B{id}` 作为链路标签。
- `--noise=PROB` 支持 `30%`、`0.05`（0.05%）、`30000000`（原始 1e8 分母）三种写法，
  范围 0..=100%；只写 `--noise` 等价于 1%。
- 省略 `--id` 时自动选择最小可用 id（上限 64 对）。
- Windows 需要**管理员权限**运行（打开总线设备接口需要写访问）。
- 排查问题时设置环境变量 `COM0COM_TRACE=1`，`setupc` 会把设备路径与每次 IOCTL 调用
  以 `[setupc]` 前缀写到 stderr，不影响正常输出。

`setupc` 是 SDK 之上的薄封装，命令解析与输出格式保持不变，后端由平台自动选择：

- **Windows**：经 `com0com-backend-win` 枚举 `GUID_COM0COM_BUS` 并发出
  `IOCTL_COM0COM_CREATE_PAIR` / `DELETE_PAIR` / `ENUM_PAIRS` / `GET_PARAMS` / `SET_PARAMS`。
- **Unix**：默认连接已运行的 `com0comd`；未检测到守护进程时退化为进程内 PTY 后端
  （生命周期随 `setupc` 结束）。可用 `COM0COM_BACKEND=native|daemon` 强制指定，
  或用 `COM0COM_DAEMON=1` 要求必须连接守护进程。
- PTY 后端不具备的能力（`exclusive`、`plugin`、非 NULL 引脚接线）返回 `Unsupported`，
  而不是静默忽略；完整能力矩阵见 [`docs/platforms.md`](docs/platforms.md)。

---

## SDK 快速上手

面向 UI / 集成方的三种用法，共用同一套领域模型与能力位；细节见
[`docs/sdk.md`](docs/sdk.md)、[`docs/jsonrpc.md`](docs/jsonrpc.md)、[`docs/c-abi.md`](docs/c-abi.md)。

**Rust facade**

```rust
use com0com_sdk::Sdk;

let sdk = Sdk::open()?;              // Windows: Win32 后端；Unix: 守护进程或进程内 PTY
println!("{:?}", sdk.capabilities());
let id = sdk.create_pair(None, &Default::default())?;
let info = sdk.list_pairs()?;
sdk.delete_pair(id)?;
```

**本地 JSON-RPC 守护进程**（Unix 长生命周期场景）

```sh
com0comd run --restore --autosave --log /var/log/com0com/com0comd.log
sdk-smoke --daemon /run/com0com/com0comd.sock
```

守护进程只监听本机 Unix socket（不监听网络），socket 权限默认 0600，
使用 `--allow-uid` 时可共享给指定用户（0660）。

**C ABI**（12 个导出函数 + 生成头文件）

```sh
cargo build -p com0com-capi --release
# 头文件由 tools/header-gen 生成，CI 校验其与实现一致
# crates/com0com-capi/include/com0com_capi.h
```

调用方按“打开 → 查询能力 → 操作 → 订阅事件 → 关闭”的顺序使用；
每对端口的数据面仍然是内核 / PTY 链路，SDK 不转发串口数据。

---

## 已实现 vs. 待办（P2 特性）

**已实现**
- 分层 SDK：`com0com-core`（领域模型/能力位）→ `com0com-sdk`（Rust facade）→
  C ABI（12 个导出函数 + 生成头文件）/ 本地 JSON-RPC 守护进程 `com0comd`；
- Unix 用户态 PTY 后端：成对 PTY、原始 termios、非阻塞泵线程、链路符号链接、
  波特率节流、噪声注入、溢出丢弃、状态持久化与 `--restore`/`--autosave`；
- 守护进程安全边界：Unix socket 本地 IPC（不监听网络）、`SO_PEERCRED`/`getpeereid`
  对端校验、`--allow-uid` 白名单、只允许打开受管端口的 monitor；
- 混合驱动骨架、PDO 枚举、兼容 ID、`PORT_LINK`/`PORT_UNLINK` 引用计数与回收；
- 完整 `IOCTL_SERIAL_*`（波特率/线路控制/字符/超时/握手/modem 状态/等待事件/统计/中断/流控等）；
- 两端口数据转发、挂起读与 `WAIT_ON_MASK` 的取消安全实现；
- `setupc` 用户态配置工具（Windows / Unix 同一命令界面）；共享协议与结构体；
- `emu_br`（波特率时序仿真）：按 `baud` + 帧格式（半比特精确计数，支持 1.5 停止位）给写路径发放字节预算；
  **实现方式**：写侧短写限速（`Information` = 本次实际投递字节数），**无 staging 缓冲、无定时器/DPC**；
  读侧不限速。已知边界：调用方需重试短写；高频小写入靠 Q16 定点 carry 累积，不会饿死。
- `emu_noise`（噪声注入）：对标上游 `sys/noise.c` 的线路噪声模型，`noise_probability`
  以 1/100_000_000 为单位。每个字节按发送端字长/校验位随机命中起始位、停止位、数据位或
  校验位：起始位错误会替换为垃圾字符并连续发送最多 511 个坏字符（`broke_chars`），
  停止位错误计帧错误，数据位错误翻转对应位并可能导致奇偶错误。使用确定性 `Xorshift32`
  （种子存在 `PortState.rng_state`，可复现）；256 字节栈上分块处理，不在内核栈开大缓冲。
  `noise_count` / `FrameErrorCount` / `ParityErrorCount` 按实际故障次数累计。
- `all_data_bits`：关闭按两端字长截断数据位；默认仍像上游一样在噪声之后应用
  `(1 << min(8, 本端字长, 对端字长)) - 1` 掩码（见 `data_mask`）。
- 与上游 com0com 的逐项行为对齐、差异和兼容性说明见
  [`docs/upstream-parity.md`](docs/upstream-parity.md)。
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
- Windows 侧基于注册表的持久化配置（对标 `HKLM\...\com0com\Parameters`）；
  当前持久化由 SDK/守护进程的状态文件提供；
- 网络扩展（远程串口对）；

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
  WDK 10.0.28000 + `wdk-sys` 0.5.1 编译通过。普通 `cargo build` 产出的是 `.dll`
  （`cargo wdk build` 才会生成并改名 `.sys`）。
- **已在测试虚拟机验证**（2026-10-21）：两套驱动在 Windows 11 24H2（testsigning）中加载运行；
  `setupc add/remove/list`、双向数据传输、自动命名、删除后的设备/`SERIALCOMM` 清理均通过。
  复现见 [`tools/vm-test/`](tools/vm-test/README.md)。
- 验证环境为 VMware + Win11 24H2 评估版；KMDF 1.33 要求 **Win11 24H2 / Server 2025 或更高**。
- 配置持久化：Windows 驱动本身仍不持久化；SDK / `com0comd` 通过状态文件
  （schema 2，原子写入，`--restore`/`--autosave`）恢复串口对，未运行守护进程时
  重启后需重新 `setupc add`。
- 内核 crate `Cargo.toml` 内含 `[profile.dev]/[profile.release] panic = "abort"`
  （内核不支持 unwind）。二者现已是独立工作区根，该 profile **会正常生效**，
  不再出现“profile 被忽略”的告警。
- 真实加载前，务必在**隔离的测试虚拟机**中开启测试签名，避免影响宿主机。

---

## 许可

MIT OR Apache-2.0（与 `windows-drivers-rs` 生态一致）。
