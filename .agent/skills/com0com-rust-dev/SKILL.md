---
name: com0com-rust-dev
description: com0com-rust（Rust 重写 Windows com0com 虚拟串口内核驱动）的入口技能。当在本项目仓库中开发/评审/测试/提交代码时使用；索引 .agent/skills/ 下由 .gitmodules 托管的第三方技能，并给出本项目硬约束（本机不编译内核、提交前必须 cargo fmt、common 零 unsafe）。
---

# com0com-rust 项目开发技能

Rust 重写的 Windows com0com（虚拟 null-modem 串口对）。混合模型：
KMDF 总线 FDO 枚举两个 WDM 端口 PDO，共享 `PairContext`（非分页池 + 内核 VA + 引用计数）。

## 项目硬约束（先读这些，再动手）

1. **本机不编译**：内核 crate（`com0combus` / `com0comport`）只能在 eWDK/测试 VM
   `cargo wdk build`。本机唯一日常允许的 cargo 命令是 `cargo fmt --all`（不链接）。
   `com0com-common` 与 `tools/setupc` 可宿主机构建（CI 的 windows-latest 上跑）。
2. **提交前必须 `cargo fmt --all`**：`.githooks/pre-commit` 会强制检查（`core.hooksPath=.githooks`）。
3. **`com0com-common` 100% 安全 Rust**：零 unsafe、零多余拷贝、`#![no_std]`，所有逻辑可宿主单测。
4. **线格式唯一**：`IOCTL_SERIAL_*`（1..=40）/私有 `IOCTL_COM0COM_*` 与全部 `SERIAL_*`
   结构体只在 `com0com-common` 定义一份，内核态与用户态（setupc）共用。
5. 项目知识体系（OpenSpec/OpenSSF/PKF/PBH）见 `.agent/knowledge/`，变更须同步文档。

## 目录速查

| 路径 | 内容 |
|------|------|
| `crates/com0com-common` | 共享定义：ioctls / serial_structs / protocol / constants / pair（环形缓冲）/ **naming**（PNP ID 编码）/ **guid**（GUID 字节↔字段转换）——均可宿主单测 |
| `crates/com0combus` | KMDF 总线驱动（PDO 枚举、pair 管理） |
| `crates/com0comport` | WDM 端口驱动（COM 命名、IRP/IOCTL） |
| `tools/setupc` | 用户态配置工具（Win32 FFI，业务逻辑零 unsafe） |
| `.agent/knowledge/` | OpenSpec + OpenSSF + PKF + PBH 四层知识体系（单一事实源） |

## 已内置的第三方技能（.gitmodules 子模块）

| 目录 | 上游 | 何时用 |
|------|------|--------|
| `.agent/skills/rust-skills/` | leonardomso/rust-skills（265 条 idiomatic Rust 规则） | 写/审 Rust：ownership、错误处理、unsafe、no_std、测试、clippy |
| `.agent/skills/anthropic-skills/` | anthropics/skills（官方） | skill-creator、docx/pdf/xlsx/pptx 文档产出、mcp-builder、webapp-testing |
| `.agent/skills/superpowers/` | obra/superpowers | TDD、systematic-debugging、writing-plans、code review、verification-before-completion、git worktrees |
| `.agent/skills/rust-agentic-skills/` | bapuqln/rust-agentic-skills（含 Rust Kernel / Lint Hunter） | 内核态 Rust：IRQL、IRP、spinlock 经验与 lint 纪律 |

> 子模块是只读参考：`git submodule update --init --recursive` 拉取；**不要**把子模块
> 内容提交进本仓库，只提交指针。

## 常用命令

```bash
# 格式化（提交前必做）
cargo fmt --all

# 本机类型检查（GNU target，无需 MSVC/WDK）
cargo +nightly-x86_64-pc-windows-gnu check

# 单测（仅 common + setupc；内核除外——CI windows-latest 上执行）
cargo test --workspace --exclude com0combus --exclude com0comport

# 内核构建（eWDK 环境）
cargo wdk build -p com0combus -p com0comport
```

## 测试现状

- `com0com-common`：IOCTL 全量公式校验、SERIAL 结构体 size/offset ABI 校验、
  协议线格式 round-trip、环形缓冲边界、naming/guid 编解码均有 `#[cfg(test)]`。
- `tools/setupc`：`parse_pair_args` / `describe_flags` / 宽字符串 & GUID 转换纯函数测试。
- 内核 crate：逻辑已下沉到 common（naming/guid）由宿主单测覆盖；WDK 部分依赖 eWDK 验证。
