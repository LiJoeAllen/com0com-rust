# Spec: pair-management — 串口对的生命周期

## 功能概述
通过总线私有 IOCTL 动态创建/删除/枚举串口对，setupc 作为用户态管理入口。

## 需求
- REQ-1 `IOCTL_COM0COM_CREATE_PAIR` 用 `CreatePairRequest{pair_id, params}` 创建一对并枚举两个 PDO。
- REQ-2 `IOCTL_COM0COM_DELETE_PAIR` 删除一对，回收 `PairContext`。
- REQ-3 `IOCTL_COM0COM_ENUM_PAIRS` 返回最多 64 对的列表。
- REQ-4 pair_id 上限 64，省略时自动取最小可用 id。
- REQ-5 端口驱动经 `PORT_LINK`/`PORT_UNLINK` 获取/归还 `PairContext` VA，并做引用计数。

## 验收场景
- AC-1 `setupc add` 后设备管理器出现两个新 COM 端口。
- AC-2 `setupc remove <id>` 后端口消失且 `PairContext` 被回收（引用计数为 0）。
- AC-3 `setupc list` 输出与内核枚举一致。

## 状态
- 现状：创建/删除/枚举/引用计数已实现。
- 关联：`brain/wiki/setupc.md`
