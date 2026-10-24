---
type: decision
status: accepted
created: 2026-10-20
---

# 决策：采用混合驱动模型（KMDF 总线 + WDM 端口）

- 上下文：需要动态枚举任意数量串口对，又要精确控制每个端口的 `IOCTL_SERIAL_*` 行为。
- 决策：总线用 KMDF（易枚举/即插即用），端口用 WDM（精细控制 SERIAL 表面）。
- 理由：KMDF 简化总线/PnP；WDM 直接暴露 SERIAL 控制面，避免 KMDF 串口类驱动的限制。
- 替代：纯 KMDF 串口类驱动（限制多）、纯 WDM（枚举繁琐）。
- 后果：需分别维护两种模型；端口驱动需 WDM 绑定（crates.io `wdk-sys` 仅 KMDF 预生成绑定，须在装 WDK 环境构建）。

---
type: decision
status: accepted
created: 2026-10-20
---

# 决策：用 `.agent` 而非 `.workbuddy` 存放 agent/文档配置

- 上下文：用户可能使用多种 AI 工具，不希望绑定单一工具。
- 决策：技能与文档体系放在仓库根 `.agent/`（工具无关）。
- 理由：可被多种 AI 编码助手复用，不锁定 WorkBuddy。
- 后果：OpenSpec/PBH 原生 CLI 默认找仓库根 `openspec/`、`docs/`；本体系放在 `.agent/knowledge/` 下，需要时用软链或移动。

---
type: decision
status: accepted
created: 2026-10-21
---

# 决策：工具链与环境入口统一到 `.env` + 加载器

- 上下文：本机路径（WDK 根目录、VMware `.vmx`、`vmrun.exe`）与凭据（客户机账号口令）
  散落在命令行参数里，换机器就要重新拼一遍；同时内核 crate 首次构建需 bindgen 解析整套
  WDK 头文件，曾在 16 核机器上把 CPU 打满到用户被迫重启。
- 决策：仓库根的 `.env` 作为团队共享模板（占位值，入库），`.env.local` 放本机私密值
  （gitignore，最后加载并覆盖），配套 `tools/load-env.ps1` 与 `tools/load-env.sh`
  两套语义一致的加载器；脚本参数缺省时回落同名环境变量。
- 理由：单一入口、可复制、可按人定制；`.env` 里默认写 `CARGO_BUILD_JOBS=2` 之类安全值，
  把「低配机不要满核编译」这条经验固化成默认值而非口头约定。
- 替代：纯文档说明（易漂移、不生效）、`direnv`（Windows 支持弱）、只在 CI 里配（本机无法复现）。
- 后果：多一层间接，需靠 `.gitignore` 保证 `.env.local` 不入库；口令一旦误写进 `.env` 会泄露，
  故约定 `.env` 只允许占位值。
- 关联：[[environment]]。

---
type: decision
status: accepted
created: 2026-10-21
---

# 决策：三 workspace 根布局（根 + bus + port），禁用 `-p` 选驱动构建

- 上下文：`com0combus`(KMDF 1.33) 与 `com0comport`(WDM) 同处一个 workspace 时，
  **任何**构建（含 `cargo build -p com0combus`、`cargo wdk build`）都在编译开始前中止于
  `multiple configurations of the WDK are detected ... { Kmdf(1,33), Wdm }`。
- 决策：两个驱动各自成为独立 workspace root，根 workspace `exclude` 它们；
  驱动一律用 `cargo build --manifest-path crates/<driver>/Cargo.toml` 构建。
- 理由（源码级，非猜测）：`wdk-build 0.5.1` 的 `src/metadata/mod.rs`
  `impl TryFrom<&Metadata> for Wdk` 把 `metadata.packages`（整张 cargo metadata，
  即 workspace 全部成员）的 `package.metadata.wdk` 与根 `workspace.metadata.wdk`
  一并收进 `HashSet<Wdk>` 并要求 `len() == 1`。它统计的是**全图**而非当前包依赖闭包，
  因此无法靠 `-p` 规避；这是上游设计约束，非本仓库实现问题。
- 替代与为何否掉：纯 KMDF 化 port（违背「WDM 精细控制 SERIAL 表面」的既有决策，见
  [[decisions]] 首条，代价是重写 IRP/WDF 对象模型）；升级 wdk-build 期望上游修复
  （当前 0.5.1 行为未变，且新版需重新验证，成本高于收益）。
- 后果：驱动 manifest 的 `[workspace.package]`/`[workspace.dependencies]` 无法继承，
  版本重复由 `tools/check-deps.ps1`（`-Fix` 可同步，pre-commit + CI 兜底）强制与根一致；
  附带收益是各驱动 `[profile.*] panic = "abort"` 真正生效。
- 关联：[[architecture]]、[[bus-driver]]、[[port-driver]]。

---
type: decision
status: accepted
created: 2026-10-24
---

# 决策：Unix 采用用户态 PTY，不做平台内核驱动

- 上下文：要求跨平台且 **0 手写 C/C++**；Linux C TTY 模块和 macOS DriverKit dext 都
  需要 C/C++ 或复杂签名/SIP 流程，并长期绑定内核 ABI。
- 决策：Windows 保留现有 Rust 内核驱动；Linux/macOS 使用纯 Rust 用户态 PTY backend。
- 理由：唯一能满足 0 C/C++ 与可维护性的路径；PTY 是 POSIX 标准接口，应用可直接打开。
- 替代与为何否掉：Linux 内核模块（C，违反约束）、DriverKit（C++/签名复杂）、
  用户态虚拟串口 `socat` 依赖（外部进程、不可控）。
- 后果：缺少 exclusive/plugin/pin wiring/驱动诊断/驱动日志；能力位如实为 false；
  PTY 生命周期随创建进程结束，因此提供长驻 `com0comd`。
- 关联：[[architecture]]、[[sdk-and-backends]]。

---
type: decision
status: accepted
created: 2026-10-24
---

# 决策：三种 SDK 形态共用能力位与领域模型

- 上下文：UI 技术栈未知，可能用 Rust、C/C++ 或脚本语言；不同平台能力不一致。
- 决策：Rust facade、C ABI、本地 JSON-RPC daemon 三种入口共用 `com0com-core`
  的领域类型与 `Capabilities`；UI 先查能力再展示功能。
- 理由：单一语义源，避免三套 API 漂移；能力位让缺失功能可预期地降级。
- 后果：新增能力必须先加核心位；C ABI 头文件由 Rust 生成并由 CI diff 校验。
- 关联：[[sdk-and-backends]]。

---
type: decision
status: accepted
created: 2026-10-24
---

# 决策：setupc 改薄 + 守护进程可选

- 上下文：setupc 原先把 FFI、业务和输出混在一个 bin，UI 无法复用；Unix PTY 需要
  长生命周期宿主。
- 决策：setupc 只保留参数解析/输出，全部经 SDK；daemon 作为可选的本地 JSON-RPC 宿主，
  提供 restore/autosave、事件与 monitor。
- 理由：保持 VM 31 项 CLI 兼容，同时给 UI 稳定的进程外接口；不强制所有用户跑守护进程。
- 后果：Unix 默认自动发现 daemon，未发现则退化进程内 PTY；`COM0COM_BACKEND`/
  `COM0COM_DAEMON` 可显式控制。
- 关联：[[setupc]]、[[sdk-and-backends]]。
