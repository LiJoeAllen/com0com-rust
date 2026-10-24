---
type: feature
status: done
last_updated: 2026-10-24
---

# 功能：用户态配置工具（setupc）

- SDK 之上的薄 CLI：命令解析/输出格式/退出码保持不变；业务逻辑全部走 `com0com-sdk`。
- Windows：经 `GUID_COM0COM_BUS` 打开总线，发私有 `IOCTL_COM0COM_*` 增删查串口对。
- Unix：默认发现 `com0comd`，否则进程内 PTY；`COM0COM_BACKEND=native|daemon` 强制模式，
  `COM0COM_DAEMON=1` 要求必须连守护进程。
- 不支持的参数（exclusive/plugin/pin wiring）返回 `Unsupported`，不静默忽略。
- 关联：`[[sdk-and-backends]]`、`[[pair-management]]`
- 关键源文件：`tools/setupc/src/{main,args,output}.rs`
- 用法：`setupc list|add|remove|params|set`
