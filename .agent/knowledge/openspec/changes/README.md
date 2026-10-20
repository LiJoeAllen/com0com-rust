# changes/ — 变更提案工作区

每个提议的变更在此建一个子目录 `<change-name>/`，至少包含：

- `proposal.md`：背景与动机、预期影响、实现思路、相关文档。
- `design.md`：技术方案与关键决策。
- `tasks.md`：可勾选的实现任务清单。
- `specs/`（可选）：本变更带来的 spec delta（需求增删改）。

审阅确认后实现；完成后用 `openspec archive`（或手动移入 `../archive/`）并将 delta 合并回 `specs/`。

> 本仓库未强制使用 OpenSpec CLI；可手工维护，也可在仓库根放 `openspec/` 后用原生 `/opsx:archive`。
