# AGENTS.md — com0com-rust 的 AI 开发行为准则

适用于在本仓库工作的任何 AI 编码助手。违反下列准则需在变更提案中显式说明理由。

## 构建与验证
- 内核驱动（`com0combus`/`com0comport`）只能在 **eWDK/测试 VM** 上用 `cargo wdk build` 构建；本机（无 MSVC/WDK）只做 `cargo +nightly-x86_64-pc-windows-gnu check` 类型检查。
- 用户已明确：**本机不编译**二进制。不要尝试链接出 `.sys`/`.exe`。
- **例外**：`cargo fmt --all`（格式化，不链接）是本机唯一日常允许的 cargo 命令，且为提交硬前提（见下）。
- 工作区 `default-members` 仅含 `com0com-common` 与 `tools/setupc`。

## 安全 / 零拷贝立场（硬约束）
- `com0com-common`：必须保持 **100% 安全 Rust**（零 unsafe、零多余拷贝）。
- `setupc`：业务命令逻辑（`cmd_*`/`main`）零 `unsafe`；unsafe 仅允许出现在 Win32 FFI helper；请求发送零拷贝（`struct_bytes` 字节视图直传 `DeviceIoControl`）。
- 内核驱动无法零 unsafe（WDK FFI/自旋锁/IRP 本质 unsafe）——**不要为“零 unsafe”改写内核 crate**；新 unsafe 也须收敛进小 helper（`read_in`/`write_out`/`lock`/`unlock`/`read_struct`/`write_struct`）。

## 私有协议
- 总线/端口/setupc 之间的控制码与结构体定义在 `com0com-common`（`ioctls.rs`/`serial_structs.rs`/`protocol.rs`），内核态与用户态共用，线格式唯一。改动须同步两端并补单测。

## IOCTL 规范
- 用户态可见 `IOCTL_SERIAL_*`（函数码 1..=40）按 WDK `ntddser.h` 顺序，`METHOD_BUFFERED`。
- 私有 `IOCTL_COM0COM_*` 仅用于总线/端口/setupc 内部管理。

## Git 提交规则（硬规则）
- **每次 `git commit` 前必须执行 `cargo fmt --all`**。本地已配置 `git config core.hooksPath .githooks`，`.githooks/pre-commit` 会以 `cargo fmt --all -- --check` 强制拦截；CI 同名步骤同样校验。
- 提交信息用 Conventional Commits（`feat:` / `fix:` / `docs:` / `chore:` / `refactor:`）。
- `.agent/skills/` 下的第三方技能均以 `.gitmodules` submodule 管理：**不要**把子模块内容当普通文件加入父仓库；只提交 gitlink 的指针更新（`git submodule update --remote` 之后 `git add .agent/skills/<name>`）。项目自有知识在 `.agent/knowledge/`，不属于 submodule。

## 提交与文档同步
- 任何代码变更若影响架构/安全/协议，必须同步更新本体系对应文档（`openspec/specs/`、`security/`、`brain/`）。
