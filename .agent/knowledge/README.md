# com0com-rust · AI 文档管理体系

本项目采用 **四支柱 AI 文档管理框架**，让任意 AI 编码助手（不绑定单一工具）都能快速、准确地获取项目上下文，并随开发演进持续沉淀知识。

| 支柱 | 定位 | 目录 | 来源 |
|------|------|------|------|
| **OpenSpec** | 规范驱动开发：能力规范 + 变更提案 + 归档，作为系统行为的权威基线 | `openspec/` | [openspec.dev](https://openspec.dev)（MIT，spec-driven dev） |
| **OpenSSF** | 开源供应链安全：安全开发框架、Scorecard、SBOM | `security/` | Open Source Security Foundation |
| **PKF** | Project Knowledge File：单文件结构化项目知识，可跨项目/会话复用 | `pkf/` | Project Knowledge File 方法论 |
| **PBH** | Project Brain Hub：面向 Agent 的知识中枢（raw/wiki/brain 三层 + SCHEMA） | `brain/` | project-brain 模式 |

## 工作流
1. **新会话开始**：Agent 先读 `README.md` → `openspec/AGENTS.md`（行为准则）→ `openspec/project.md`（全景）→ `brain/SCHEMA.md`（读写规则）。
2. **接到任务**：在 `openspec/specs/` 找能力规范，在 `brain/wiki/` 找功能现状，在 `brain/brain/` 找跨切面的决策/风险。
3. **提出变更**：在 `openspec/changes/<name>/` 写 `proposal.md` + `design.md` + `tasks.md`，审阅后实现，完成后归档（`archive/`）。
4. **知识沉淀**：决策写进 `brain/brain/decisions.md`（append-only），风险写进 `risks.md`，里程碑写进 `milestones.md`；同一事实只在单一层存真。

## 与 `.agent/skills/com0com-rust-dev` 的关系
技能文件是“给 Agent 的速查指令”，本文档体系是“项目的活文档”。技能里提到的架构/构建/安全要点，以本文档体系的 `openspec/`、`security/`、`brain/` 为准（单一事实源）。

## 工具说明
- OpenSpec 原生 CLI（`openspec init` / `/opsx:*`）默认找仓库根的 `openspec/`。本体系放在 `.agent/knowledge/openspec/` 以贴合“工具无关”约定；若要用原生 CLI，可在仓库根建 `openspec/` 软链或移动本目录。
- PBH 原生放 `docs/`，同理放在 `.agent/knowledge/brain/`。
- PKF 是单文件知识产物，放在 `pkf/`，可直接复制到其他项目复用。

## 环境配置
新机器 clone 后按仓库根 `.env` 补齐本机路径与凭据即可运行；`tools/vm-test/` 下的脚本
会**自动加载** `.env` / `.env.local`，直接跑 `cargo` 时再手动 dot-source
（`tools/load-env.ps1` / `tools/load-env.sh`）。私密值写 `.env.local`（已 gitignore）。
完整变量清单与路径解析规则见 [brain/wiki/environment.md](./brain/wiki/environment.md)。

## 构建与 workspace 布局（先读，否则会踩坑）
仓库有**三个** workspace 根（根 + `crates/com0combus` + `crates/com0comport`），
这是 `wdk-build` 的硬约束：整张 cargo metadata 里只能出现一种驱动模型配置，
KMDF/WDM 混用会让**任何**构建在编译前中止。因此：
- 驱动**不能** `cargo build -p com0combus`，只能用
  `cargo build --manifest-path crates/com0combus/Cargo.toml`（port 同理）
- 依赖版本只在根 `Cargo.toml` 手写，副本由 `tools/check-deps.ps1 -Fix` 同步

完整依据见 [brain/brain/architecture.md](./brain/brain/architecture.md)
与 [brain/brain/decisions.md](./brain/brain/decisions.md) 的「三 workspace 根布局」条目。

## 流程与治理
- **分支管理**：见 [BRANCHING.md](./BRANCHING.md) — Trunk-Based 分支模型、命名规范、与 OpenSpec 变更的对应、分支保护（OpenSSF Scorecard 合规）。
