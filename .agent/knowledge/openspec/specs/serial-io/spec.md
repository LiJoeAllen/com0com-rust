# Spec: serial-io — 串口 I/O 与 SERIAL 控制面

## 功能概述
端口驱动向 Win32 暴露标准串口（COM）行为：打开/关闭、读写、全部 `IOCTL_SERIAL_*`（波特率、线控、握手、超时、等待事件掩码、状态查询、清除/刷新等）。

## 需求（Requirement）
- REQ-1 必须实现函数码 1..=40 的全部 `IOCTL_SERIAL_*`，语义对齐 WDK `ntddser.h`。
- REQ-2 所有控制码使用 `METHOD_BUFFERED`。
- REQ-3 读/写在对方无数据时须可挂起（pending IRP），并在对端写入、关闭或取消时正确完成/取消。
- REQ-4 环缓冲在两端间转发字节，须由 `PairContext` 自旋锁保护，避免竞争。
- REQ-5 参数（波特率、线控、握手、超时等）须正确存储并影响行为（仿真类参数在 P2 阶段部分仍为 stub，见 `brain/brain/risks.md`）。

## 验收场景（Acceptance）
- AC-1 setupc 创建一对后，两端 COM 端口均可被 `CreateFile` 打开。
- AC-2 A 端写入的字节可由 B 端 `ReadFile` 完整读出，顺序与内容一致。
- AC-3 B 端关闭后，A 端挂起的读 IRP 须被取消完成（STATUS_CANCELLED）。
- AC-4 `SetCommState`/`GetCommState` 往返一致。

## 状态
- 现状：骨架与核心转发已实现；部分仿真参数（REQ-5 子集）为 stub。
- 关联变更：`openspec/changes/`（待建）
