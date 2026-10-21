---
type: feature
status: done
last_updated: 2026-10-21
---

# 功能：环境配置（.env 清单 + 加载器）

新机器 clone 后按 `.env` 逐项补齐即可运行，**不需要在脚本里硬编码本机路径**。

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

## 关键的坑（`E0080` 与 libclang 版本）
`wdk-sys` 结构体 size 退化成 1 的根因是 **libclang 22.1.x 回归**，不是缺 WDK：
`clang_getTypeDeclaration` 对 `typedef struct X X; struct X {...};` 返回前向声明而非
定义。解法用 `.env` 里的 `LIBCLANG_PATH`（指向 18.1.1）+ `BINDGEN_EXTRA_CLANG_ARGS`
（补 SDK 的 shared/ucrt/um，路径不能含空格，先建 junction）。

## 用法
```powershell
. tools/load-env.ps1                    # PowerShell 必须 dot-source
pwsh -File tools/vm-test/run-vm-test.ps1
```
```bash
source tools/load-env.sh                # bash 必须 source
cargo build
```
只想核对生效值时：`bash tools/load-env.sh --print` / `pwsh -File tools/load-env.ps1 -Print`。

## 已收录的变量
- 构建：`WDKContentRoot`、`CARGO_BUILD_JOBS`
- 虚拟机端到端测试：`VMRUN_VMX_PATH`、`VMRUN_GUEST_USER`、`VMRUN_GUEST_PASSWORD`、
  `VMRUN_PATH`、`VMRUN_SNAPSHOT_NAME`、`VM_TEST_CONFIGURATION`、`VM_TEST_JOBS`

## 关联
- `tools/vm-test/run-vm-test.ps1`：参数缺省时回落同名环境变量。
- `tools/vm-test/build-package.ps1`：直接读 `WDKContentRoot` 与 `CARGO_BUILD_JOBS`。
- 参见 [[bus-driver]]、[[port-driver]]、[[setupc]] 的运行前置条件。
- 决策见 [[decisions]]「工具链与环境入口」一条。
