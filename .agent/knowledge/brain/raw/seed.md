---
type: vision
status: done
last_updated: 2026-10-20
---

# 种子记录（raw）

原始背景：用户希望用 Rust 重写 com0com，并建立一套工具无关的 AI 文档管理体系（OpenSpec + OpenSSF + PKF + PBH），采用 `.agent` 目录约定而非 `.workbuddy`。

- 本仓库刚 `git init`，文档体系由本会话搭建。
- 硬约束：本机不编译内核；零 unsafe（业务）/ 零拷贝（输入）。
