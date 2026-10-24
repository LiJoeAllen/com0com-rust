# 任务清单

## Phase 0：workspace 脚手架

- [x] 新增 `crates/com0com-{core,backend-win,backend-pty,backend-stub,sdk,ipc,daemon,capi}` 与 `tools/{sdk-smoke,capi-smoke,header-gen}` 骨架
- [x] 根 `Cargo.toml` 登记 members/default-members/workspace.dependencies
- [x] `.cargo/config.toml` 的 `crt-static` 限定为 Windows 目标
- [x] `tools/check-deps.ps1` 纳入新成员
- [x] CI 增加 Linux/macOS check 与纯 Rust 源码校验

## Phase 1：com0com-core

- [x] 领域模型、能力位、错误、事件、`Backend`/`MonitorHandle` trait
- [x] 状态快照/恢复选项/报告模型
- [x] `fake` feature 内存后端与单测

## Phase 2：Windows backend + setupc

- [x] IOCTL transport、设备枚举、参数转换、诊断、事件、端口列表
- [x] 最小 SDK 门面
- [x] setupc 拆分 args/output 并保持文案/退出码
- [x] VM 31/31 回归

## Phase 3：完整 Rust SDK

- [x] 事件总线、Monitor、StateApi
- [x] fake 测试：过滤、并存订阅、回环、背压、原子写、restore 报告

## Phase 4：IPC + daemon

- [x] JSON-RPC DTO/分帧/RemoteBackend/服务端分发
- [x] `com0comd` named pipe、ACL、单实例、restore/autosave
- [x] fake backend 起真实管道集成测试

## Phase 5：C ABI

- [x] `com0com-capi` 导出与错误码
- [x] `tools/header-gen` 生成头文件快照
- [x] 布局测试与 `capi-smoke --self-test`

## Phase 6：Windows 收尾 + VM + 文档

- [x] driver_logs（EvtQuery/XML）、版本与 SHA-256
- [x] VM 打包/guest 断言、SDK/C ABI/RPC 冒烟（47/47，含波特率仿真写路径；
      驱动哈希一致，`--restore`/`--autosave` 实测；日志
      `target/vm-test-results/20261024-034210/`）
- [x] README、docs、OpenSpec、brain 同步

## Phase 7：Unix PTY

- [x] PTY 引擎、termios raw、pump、背压、shutdown
- [x] daemon、Unix socket、peer creds、`/dev` 链接与用户目录回退
- [x] 波特率/噪声/overrun 仿真
- [x] `check-pairs-unix.sh` 与 CI `unix-e2e` 作业
- [x] Linux PTY E2E（WSL2 + `x86_64-unknown-linux-musl`：非 root 51/51，
      root 60/60，后者含 `/dev/com0com` 默认链接、双向传输与状态文件 0600；
      日志 `target/vm-test-results/unix-wsl-20261024-0245/`）
- [x] macOS 路径/权限审计：共享 `/dev` 不做 chmod、用户目录 0700、状态文件 0600、
      socket 目录 0700、`relabel` 回滚绝不删除普通文件；Linux/macOS 全目标 clippy 通过
- [x] VM 编排失败闭合：客户机未就绪或本次未执行 `check-pairs.ps1` 时直接判 FAIL，
      不再解析客户机遗留日志；就绪超时默认提高到 1200 秒；回滚前校验快照类型，
      带内存状态的快照直接拒绝（实测回滚后会停在锁屏且 Tools 不起）
- [ ] macOS PTY E2E（CI `unix-e2e` workflow_dispatch，待运行）

## 验证

- [x] `cargo fmt --all -- --check`
- [x] `cargo clippy --workspace --all-targets -- -D warnings`（含 Linux-musl / macOS 交叉目标）
- [x] `cargo test --workspace`（Windows 宿主）
- [x] Linux-musl 测试二进制在 WSL2 全部通过（15 套件 / 175 项，日志
      `target/vm-test-results/linux-tests-20261024-031558.log`）
- [ ] macOS 宿主 `cargo test`（目前仅交叉编译检查，随 macOS E2E 一并执行）
- [x] `pwsh tools/check-deps.ps1 -Fix` 无 diff
- [x] 纯 Rust 源码检查（无手写 C/C++）
- [x] Windows VM 31 项 + SDK/C ABI/RPC 冒烟（实际 47 项断言全过，含波特率
      仿真写路径，日志 `target/vm-test-results/20261024-034210/`）
- [x] Linux PTY E2E（WSL2：非 root 51/51 + root 60/60，日志
      `target/vm-test-results/unix-wsl-20261024-0245/`）
- [ ] macOS PTY E2E（CI workflow_dispatch）
- [x] 安全复查与文档一致性（无网络监听、`serial.open` 白名单、root/用户默认路径、
      socket 目录 0700、状态文件 0600、C ABI 文档命令已校正）
