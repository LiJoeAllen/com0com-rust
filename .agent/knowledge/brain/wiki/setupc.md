---
type: feature
status: done
last_updated: 2026-10-20
---

# 功能：用户态配置工具（setupc）

- 经 `GUID_COM0COM_BUS` 打开总线，发私有 `IOCTL_COM0COM_*` 增删查串口对。
- 业务命令逻辑零 unsafe；输入零拷贝（`struct_bytes`）。
- 关联：`[[pair-management]]`
- 关键源文件：`tools/setupc/src/main.rs`
- 用法：`setupc list|add|remove|params|set`
