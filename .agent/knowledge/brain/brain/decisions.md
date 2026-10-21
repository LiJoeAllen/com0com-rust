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
