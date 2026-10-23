---
type: feature
status: done
last_updated: 2026-10-23
---

# 功能：环境配置（.env 完整清单 + 加载器）

新机器 clone 后按 `.env` 逐项补齐即可运行，**不需要在脚本里硬编码本机路径**。
`.env` 是构建环境与 VM 测试的**完整清单**；`tools/vm-test/` 下的脚本会自动加载，
不必先手动 source。

## 三个文件各司其职
- `.env`（入库）：团队共享模板，只有占位值与安全默认值，**禁止写真实口令**。
- `.env.local`（`.gitignore`）：本机私密值，后加载并覆盖 `.env`。
- `tools/load-env.ps1` / `tools/load-env.sh`：`pwsh`/`bash` 两套加载器，语义一致。

## 取值优先级
命令行显式传参 → `.env.local` → 已有环境变量 → `.env` → 脚本内置默认值。

刻意让 `.env.local` **压过**已有环境变量，`.env` 则不抢：机器上常有遗留同名变量挡路，
最典型的是系统装了 libclang 22（`LIBCLANG_PATH` 指向 `D:\LLVM\bin`），而本仓库内核
crate 必须用 18——22.1.x 的 bindgen 回归会让 wdk-sys 编译爆成片 `E0080`。
只有项目级显式配置（`.env.local`）能纠正它；而 `.env` 作为共享模板不夺权，以免
干扰 CI 与临时 `env FOO=bar`。

## 自动加载
- `tools/vm-test/build-package.ps1` 与 `run-vm-test.ps1` 启动时 dot-source
  `tools/load-env.ps1 -Quiet`，因此 `.env.local` 填好后可以直接：
  `pwsh -File tools/vm-test/run-vm-test.ps1`。
- 命令行显式传参仍然优先：脚本在加载前先记录 `$PSBoundParameters`（dot-source
  会覆盖该自动变量）。
- 加载器把 `WDK_BIN_PATH` 前置到 `PATH`，避免 Git Bash/MSYS 同名 `signtool`
  抢先命中（会报 `unrecognized option: sign`）。

## 路径解析
- 输入路径（`LIBCLANG_PATH`、`WDKContentRoot`、`DEVCON_PATH`、`WDK_BIN_PATH`、
  `VMRUN_PATH`）的相对值在目标存在时锚定到仓库根。
- 输出目录（`VM_TEST_PACKAGE_DIR`、`VM_TEST_RESULTS_DIR`）的相对值**无条件**
  锚定到仓库根（目录可能尚不存在）。
- `VM_TEST_GUEST_DIR` 是客户机路径，原样传给 `vmrun`，不做主机路径解析。
- `BINDGEN_EXTRA_CLANG_ARGS` 中每个 `-I` 的目标路径同样锚定；该变量按空白切分，
  路径不能含空格（含空格请先建 junction）。

## 关键的坑（`E0080` 与 libclang 版本）
`wdk-sys` 结构体 size 退化成 1 的根因是 **libclang 22.1.x 回归**，不是缺 WDK：
`clang_getTypeDeclaration` 对 `typedef struct X X; struct X {...};` 返回前向声明而非
定义。解法用 `.env` 里的 `LIBCLANG_PATH`（指向 18.1.1）+ `BINDGEN_EXTRA_CLANG_ARGS`
（补 SDK 的 shared/ucrt/um，路径不能含空格，先建 junction）。

## 用法
```powershell
. tools/load-env.ps1                    # PowerShell 必须 dot-source；-Quiet 可静默
pwsh -File tools/vm-test/run-vm-test.ps1
```
```bash
source tools/load-env.sh                # bash 必须 source
cargo build
```
核对生效值：`bash tools/load-env.sh --print` / `pwsh -File tools/load-env.ps1 -Print`
（`VMRUN_GUEST_PASSWORD` 会打码为 `***`）。

## 已收录的变量

### 构建
| 变量 | 说明 |
|---|---|
| `WDKContentRoot` | WDK 内容根；留空自动探测 |
| `CARGO_BUILD_JOBS` | cargo 并行度，默认 2 |
| `LIBCLANG_PATH` | libclang 18 的 native 目录 |
| `BINDGEN_EXTRA_CLANG_ARGS` | 补 SDK 的 `shared/ucrt/um` include 路径 |
| `WDK_BIN_PATH` | 含 `signtool.exe` 的 SDK bin 目录；前置到 `PATH` |
| `DEVCON_PATH` | `devcon.exe` 完整路径；留空自动查找 |
| `COM0COM_TRACE` | `setupc` 诊断开关，非空开启并写 stderr |

### 虚拟机端到端测试
| 变量 | 说明 |
|---|---|
| `VMRUN_VMX_PATH` | 测试虚拟机 `.vmx` 路径 |
| `VMRUN_GUEST_USER` / `VMRUN_GUEST_PASSWORD` | 客户机管理员凭据（只写 `.env.local`） |
| `VMRUN_PATH` | `vmrun.exe` 路径；留空自动查找 |
| `VMRUN_SNAPSHOT_NAME` | 回滚用快照名（须为关机状态） |
| `VM_TEST_CONFIGURATION` | `debug` / `release` |
| `VM_TEST_JOBS` | 构建并行度，留空回落 `CARGO_BUILD_JOBS` |
| `VM_TEST_PACKAGE_DIR` | 测试包输出目录 |
| `VM_TEST_RESULTS_DIR` | 测试日志输出目录 |
| `VM_TEST_GUEST_DIR` | 客户机工作目录 |
| `VM_TEST_BOOT_TIMEOUT_SEC` | 等待客户机就绪的超时秒数（默认 600） |

## 关联
- `tools/vm-test/run-vm-test.ps1`：连接/快照/输出/超时参数缺省时回落同名环境变量。
- `tools/vm-test/build-package.ps1`：自动加载 `.env`，读 `DEVCON_PATH`、`VM_TEST_JOBS`、
  `CARGO_BUILD_JOBS`，并把 `WDK_BIN_PATH`（或自动探测的 SDK bin）前置到 `PATH`。
- 参见 [[bus-driver]]、[[port-driver]]、[[setupc]] 的运行前置条件。
- 决策见 [[decisions]]「工具链与环境入口」一条。
