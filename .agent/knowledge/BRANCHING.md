# 分支管理（Branch Management）

适用于 com0com-rust。本策略贴合 OpenSpec 的轻量、迭代哲学，并满足 OpenSSF Scorecard 的 Branch-Protection 要求。

## 默认分支
- `master`：已设为默认（`git init -b master`）。代表**可发布 / 可加载**的稳定状态。
- 保护：`master` 禁止直接 push，仅通过 PR 合并；需 ≥1 评审；需状态检查通过；禁止 force-push；合并后自动删头部分支。

## 分支模型：Trunk-Based + 短生命周期特性分支
| 分支类型 | 命名 | 说明 |
|----------|------|------|
| 主干 | `master` | 稳定、受保护，仅通过 PR 合并。 |
| 特性 | `feature/<name>` | 新能力 / 重构；生命周期尽量短（天级）。对应一个 `openspec/changes/<name>/`。 |
| 修复 | `fix/<name>` | Bug 修复。 |
| 文档 | `docs/<name>` | 文档 / 知识体变更（如更新本体系）。 |
| 实验 | `experiment/<name>` | 高风险探索，可丢弃。 |
| 发布 | `release/<version>` | 用于签名 / 发布驱动与 setupc 的稳定快照；从 `master` 切出，仅接 `fix/` 回溯，打 tag 后合回 `master`。 |

## OpenSpec 与分支的对应
- 每个 OpenSpec 变更（`openspec/changes/<name>/`）在其专属分支 `feature/<name>` 上开发。
- 审阅通过后：实现并入 `master`，同时把变更归档（`openspec/archive/`），spec delta 合并回 `openspec/specs/`。
- 一分支一变更，避免长生命周期大分支（OpenSpec 反对瀑布式大改）。

## 提交与评审规则
- 提交信息采用 Conventional Commits（`feat:` / `fix:` / `docs:` / `chore:` / `refactor:`），与 setupc 的 DefaultPrompts 风格一致。
- **硬规则：每次 `git commit` 前必须 `cargo fmt --all`**（本地 `.githooks/pre-commit` 强制拦截；CI 的 `fmt check` 步骤同样校验）。
- PR 必须关联对应 `openspec/changes/<name>/`（若无则先建提案）。
- 涉及架构 / 安全 / 协议的 PR，须同步更新 `.agent/knowledge/` 文档（单一事实源）。
- 内核驱动改动须注明“已在 eWDK 通过 `cargo wdk build`”（本机无法验证）。

## 分支保护（在 GitHub / 代码平台配置）
- `master`：Require PR before merging；Required approvals ≥ 1；Require status checks（`cargo check` CI、OpenSSF Scorecard）；不允许 bypass；Block force push；Delete head branch after merge。
- `release/*`：同 `master` 评审要求，但允许只接 `fix/` 回溯。
> Scorecard 的 Branch-Protection 检查即验证上述配置（见 `security/scorecard.md`）。

## 初始引导（首次提交后执行）
```bash
git add -A && git commit -m "chore: bootstrap repo with .agent knowledge system"
git branch develop            # 可选：若想要 develop 缓冲层
git remote add origin <your-repo-url>
git push -u origin master
# 在代码平台 Settings → Branches 配置上述保护规则
```

## 标签
- 发布用语义化 tag：`v0.1.0` 等，打在 `release/<version>` 合回 `master` 的提交上；驱动签名产物随 tag 归档。
