# OpenSSF Scorecard 跟踪

Scorecard 对仓库做风险打分。本项目建议跟踪的维度：

| 检查项 | 目标 | 现状 |
|--------|------|------|
| Branch-Protection | 主分支保护 + 需评审 | 待配置（仓库刚 `git init`） |
| Token-Permissions | 最小权限 | 待配置 |
| Dependency-Update-Tool | 自动依赖更新 | 待启用（Dependabot/renovate） |
| Signed-Releases | 发布产物签名 | 驱动需 EV 签名（见 `ssdf.md`） |
| Dangerous-Workflow | 无危险 workflow | 待添加 CI |
| Binary-Artifacts | 不提交二进制 | 遵守（`.sys`/`.exe` 由 CI 产出） |

运行：`scorecard --repo=.:` 或 GitHub Action。
