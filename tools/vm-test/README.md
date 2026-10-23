# 虚拟机验证流程

本目录把 com0com-rust 的**驱动构建 → 部署 → 安装 → 串口互通验证**固化为可重复执行的脚本。
验证在隔离的 Windows 测试虚拟机中进行，不接触宿主机的驱动与证书存储。

```
tools\vm-test\
  build-package.ps1        构建驱动与 setupc，组装测试包
  run-vm-test.ps1          宿主机编排：快照/启动 VM、复制、安装、重启、收集日志
  guest\install-bus.ps1    客户机：校验签名环境、导入测试证书、安装 com0combus
  guest\install-port.ps1   客户机：清理旧 com0comport 包并安装本次构建
  guest\check-pairs.ps1    客户机：串口对生命周期与双向传输断言
```

## 前置条件

### 构建机
- Windows + **WDK 10.0.28000** + **Windows SDK 10.0.26100**；
- Rust stable + `cargo-wdk`（`cargo install cargo-wdk`）；
- libclang 18–20，按根 [README](../../README.md) 配好：
  - `LIBCLANG_PATH`
  - `BINDGEN_EXTRA_CLANG_ARGS`（补 SDK 的 `shared`/`ucrt`/`um` include 路径）
- `devcon.exe`（WDK Tools 自带；通过 `DEVCON_PATH` / `-DevconPath` 指定，或在 `Windows Kits\10\Tools\*\x64\` 下自动查找）。
- 含 `signtool.exe` 的 SDK bin 目录会被自动前置到 `PATH`；也可用 `WDK_BIN_PATH` 显式指定，
  避免 Git Bash/MSYS 的同名 `signtool` 抢先命中。

> 以上路径与凭据都可以写在仓库根的 `.env.local`（模板见 `.env`）。本目录的两个脚本
> **会自动加载** `.env` / `.env.local`，命令行显式传参仍然优先。

### 测试虚拟机
- VMware Workstation + VMware Tools；
- 客户机 **Windows 11 24H2 / Server 2025 或更高**（KMDF 1.33 的硬性要求）；
- 管理员账户；
- **Secure Boot 关闭**、**testsigning 开启**：
  ```powershell
  bcdedit /set testsigning on
  shutdown /r /t 0
  ```
  重启后 `bcdedit /enum {current}` 应显示 `testsigning Yes`。
- 建议关机后创建快照（例如 `clean`），每次验证前回滚，避免 DriverStore 里的旧驱动包干扰。

> ⚠️ **安全边界**：测试包由自签证书 `WDRLocalTestCert` 签名，安装脚本会把包内 `.cer`
> 导入客户机的 `Root` 与 `TrustedPublisher`。**只能在一次性/隔离的测试虚拟机中这样做**，
> 绝不要在生产机或日常使用的机器上导入该证书或开启 testsigning。

## 1. 构建测试包

```powershell
# 配好 .env.local 后可直接运行（DEVCON_PATH 等也已在其中）
powershell -ExecutionPolicy Bypass -File tools\vm-test\build-package.ps1

# 需要临时覆盖时用命令行传参
powershell -ExecutionPolicy Bypass -File tools\vm-test\build-package.ps1 `
  -Configuration release `
  -DevconPath 'C:\Program Files (x86)\Windows Kits\10\Tools\10.0.26100.0\x64\devcon.exe'
```

`-DevconPath` 可省略（回落 `DEVCON_PATH` 或自动查找）；`-OutputDir` 可省略
（回落 `VM_TEST_PACKAGE_DIR` 或 `target\vm-test-package`）。产物默认在 `target\vm-test-package\`：

```
bus\com0combus.inf|sys|cat
bus\com0comport.inf|sys|cat
setupc.exe
devcon.exe
WDRLocalTestCert.cer
SHA256SUMS.txt
```

脚本会依次构建 `setupc`、`com0combus`、`com0comport`；并行度默认 2
（`-Jobs` / `VM_TEST_JOBS` / `CARGO_BUILD_JOBS` 依次覆盖），避免低配机因并行编译内核 crate 而卡死。

## 2. 执行验证

```powershell
# .env.local 填好 VMRUN_* 后，直接运行（默认先构建测试包）
powershell -ExecutionPolicy Bypass -File tools\vm-test\run-vm-test.ps1

# 连接参数仍可用命令行覆盖
powershell -ExecutionPolicy Bypass -File tools\vm-test\run-vm-test.ps1 `
  -VmxPath 'F:\VMs\com0com-test\com0com-test.vmx' `
  -GuestUser '<客户机用户名>' `
  -GuestPassword '<客户机密码>' `
  -SnapshotName clean
```

| 参数 | 必填 | 环境变量回落 | 说明 |
|---|---|---|---|
| `-VmxPath` | 环境或参数 | `VMRUN_VMX_PATH` | 测试虚拟机 `.vmx` 路径 |
| `-GuestUser` / `-GuestPassword` | 环境或参数 | `VMRUN_GUEST_USER` / `VMRUN_GUEST_PASSWORD` | 客户机管理员凭据（vmrun 使用） |
| `-SnapshotName` | 否 | `VMRUN_SNAPSHOT_NAME` | 指定时先关机回滚快照再启动；快照必须处于关机状态 |
| `-Configuration` | 否 | `VM_TEST_CONFIGURATION` | `debug`（默认）或 `release` |
| `-PackageDir` | 否 | `VM_TEST_PACKAGE_DIR` | 默认 `target\vm-test-package` |
| `-ResultsDir` | 否 | `VM_TEST_RESULTS_DIR` | 默认 `target\vm-test-results\<时间戳>` |
| `-GuestDir` | 否 | `VM_TEST_GUEST_DIR` | 客户机工作目录，默认 `C:\com0com-vmtest` |
| `-VmrunPath` | 否 | `VMRUN_PATH` | 默认从 PATH 或 `Program Files\VMware\VMware Workstation` 查找 |
| `-SkipBuild` | 否 | — | 跳过构建，直接使用现有测试包 |
| `-SkipReboot` | 否 | — | 安装后不重启（默认重启，确保新驱动重新加载） |
| `-BootTimeoutSec` | 否 | `VM_TEST_BOOT_TIMEOUT_SEC` | 等待客户机就绪的超时，默认 600 秒（冷启动可能超过 5 分钟） |
| `-Jobs` | 否 | `VM_TEST_JOBS` → `CARGO_BUILD_JOBS` | 传给 cargo 的 `CARGO_BUILD_JOBS`，默认 2 |

指定 `-SnapshotName` 时，如果虚拟机正在运行，脚本会先 `vmrun stop ... hard`
再回滚（测试 VM 的运行状态会被丢弃），这是推荐用法：每轮从干净快照开始。

执行流程：

1. 可选地回滚并启动虚拟机，等待 VMware Tools 与 PowerShell 就绪；
2. 把测试包（`bus\`、`setupc.exe`、`devcon.exe`、证书、`SHA256SUMS.txt`）与客户机脚本复制到 `C:\com0com-vmtest`；
3. `install-bus.ps1`：校验管理员/Secure Boot/testsigning（未开启则自动开启并返回退出码 2，宿主机重启后重跑）、导入测试证书、删除旧总线设备树与所有 `com0combus` 包、`devcon install` 一个全新总线实例、校验设备状态与运行中哈希；
4. `install-port.ps1`：删除 DriverStore 里所有旧的 `com0comport` 包，再添加本次构建，并校验 DriverStore 中的 `com0comport.sys` 哈希；
5. 重启客户机并等待就绪；
6. `check-pairs.ps1`：创建/删除串口对、双向传输、自动命名、清理，并校验**运行中的驱动哈希**；
7. 把日志拷回宿主机的 `target\vm-test-results\<时间戳>\`，解析 `RESULT|` 行，失败时退出码为 1。

`vmrun` 的凭据参数必须放在命令前（`-gu`/`-gp`）。`runProgramInGuest` 不返回 stdout，
因此所有客户机脚本都写日志文件，由宿主机拷回后分析。

## 3. 验证内容

`check-pairs.ps1` 逐项断言并写入 `PASS|` / `FAIL|`：

| 阶段 | 检查 |
|---|---|
| 准备 | 删除所有已有 pair；清理残留的端口 phantom devnode；确认目标 COM 名空闲 |
| 显式命名 | `setupc add --port-a COM15 --port-b COM16`；两端口 `Status=OK / CM_PROB_NONE`；双向 4096 字节回环逐字节一致 |
| 驱动版本 | 运行中的 `com0combus.sys` / `com0comport.sys` SHA-256 与本次构建一致 |
| 删除 | `setupc remove 0`；端口从 `GetPortNames()` 消失；设备变 `CM_PROB_PHANTOM`；`SERIALCOMM` 条目被清除 |
| 第二对 | id 5 / COM20/COM21；双向 1024 字节一致 |
| 自动命名 | 不指定名字创建 id 7；两个端口拿到**不同**的 `COMx`；删除后清理干净 |
| 收尾 | `setupc list` 显示 `(no pairs)` |

固定使用 pair id 0/5/7 与 COM15/16/20/21，这些名字必须在客户机空闲。

## 4. 日志与排查

- 客户机日志：`C:\com0com-vmtest\install-bus.log`、`install-port.log`、`check-pairs.log`
- 宿主机日志：`target\vm-test-results\<时间戳>\`
- **驱动哈希不匹配**：旧 DriverStore 包未被删除；看 `install-port.log` 里的 `packages|`、`delete|`、`store|`。
  总线哈希不匹配时，从干净快照重来，或手动删除 `com0combus` 包后重装。
- **端口不出现 / 自动命名重复**：看 `check-pairs.log` 的 `ports|`、`driver|`、`serialcomm|`；
  自动命名依赖 Ports 类 co-installer 写入的 `Device Parameters\PortName`。
- **测试用 COM 名被占用**：`COM15/16/20/21` 必须是客户机空闲端口（固定写死在 `check-pairs.ps1` 中）。
- **等待客户机超时**：确认 VMware Tools 在运行、客户机已启动完成；增大 `-BootTimeoutSec`。
- **vmrun 报 “requires valid user name and password”**：`-gu`/`-gp` 必须在子命令之前。
- **Secure Boot 未关闭 / testsigning 未开启**：`install-bus.log` 会明确报错，驱动无法加载。

## 5. 已知限制

- 安装阶段会删除客户机中所有 `com0combus`/`com0comport` 驱动包并重建总线设备树（仅限该测试 VM）。
- 客户机需要管理员权限；UAC 关闭或使用内置管理员账户可避免 `pnputil` 被拦。
- 快照回滚会清掉 VM 内所有改动，驱动每轮重新安装，这是推荐做法。
