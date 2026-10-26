# 与上游 com0com 的行为对齐

参考实现：[`mamanakis/com0com`](https://github.com/mamanakis/com0com)（原 SourceForge
com0com 的完整 CVS 导入，C/C++ + WDM 驱动）。本项目是独立的 Rust 重写，只把上游当作
**行为规范**阅读，不复制代码；上游为 GPL-2.0，本仓库为 MIT OR Apache-2.0，二者保持
干净室边界。上游未公开文档的细节（噪声算法、引脚默认值、掩码顺序）以代码为准，
下面记录已经对齐的部分和有意保留的差异。

## 已对齐的行为

| 主题 | 上游实现 | 本项目实现 |
|---|---|---|
| 噪声概率标度 | `C0C_PROBABILITY_ONE = 100000000`，`prob > RAND() % ONE` | `NOISE_PROBABILITY_ONE = 100_000_000`，`Xorshift32::below` 后可复现 |
| 噪声命中槽位 | `sys/noise.c`：起始位、停止位、`WordLength` 个数据位、校验位各占一槽 | `emulation::broke_char` 按同样槽位选择 |
| 起始位错误 | 替换为垃圾字符，并连续发送 `RAND()%512` 个坏字符 | `broke_chars` 持久化在 `PortState`，跨写/读调用保持 |
| 停止位错误 | 帧错误 + 坏字符串 | `NoiseReport::frame_errors` |
| 数据位错误 | 翻转对应位；奇/偶校验时报奇偶错误 | 同左，`parity == ODD/EVEN` 时计数 |
| 垃圾字符 | 奇偶位按 1/2 概率翻转、帧错误按 1/2 概率、数据随机 | `garbage_char` 同分布 |
| 数据位掩码 | `sys/bufutils.c` 在噪声**之后**执行 `curChar &= dataMask` | `apply_data_mask` 在 `inject_line_noise` 之后调用 |
| `AllDataBits` | 有效位宽取 `min(8, 本端字长, 对端字长)`，开启后不截断 | `emulation::data_mask`，参数为 `all_data_bits` |
| 引脚默认接线 | CTS=RRTS、DSR=RDTR、DCD=RDTR、RI=!on（恒低） | `PinWiring::NULL_MODEM = [CTS, DSR\|DCD, 0, 0]` |
| `GET_MODEMSTATUS` | 只返回对端输出映射出的 CTS/DSR/RLSD/RING | `readwrite::modem_inputs_locked` 只派生输入线 |
| `SET_MODEM_CONTROL` | 不带私有扩展头时，按整字覆盖 DTR/RTS/OUT1/OUT2 | `set_modem_lines` 用 `mask` 同时置位/清零 |
| `GET_MODEM_CONTROL` | 返回全部 MCR 输出线；`GET_DTRRTS` 只返回 DTR/RTS | 合并实现，按 IOCTL 选择掩码 |
| 控制线事件 | `SetModemStatus` 触发 `SERIAL_EV_CTS/DSR/RLSD/RING` | 变化后按事件位唤醒对端 `WAIT_ON_MASK` |
| 概率配置入口 | `setupc set` 使用原始 1e8 标度 | `setupc --noise=30%`、`0.05`、原始整数三种写法 |

## 数据路径上的对应关系

- **噪声与掩码**：`com0com-common/src/emulation.rs` 是唯一实现，Windows 端口驱动
  （`com0comport/src/readwrite.rs`）与 Unix PTY 泵线程（`com0com-backend-pty/src/pty.rs`）
  共用它，避免两套行为漂移。
- **字长/校验位来源**：Windows 取双方 `SERIAL_LINE_CONTROL`；PTY 用 `tcgetattr`
  读取两端持有 fd 的 `CSIZE`/`PARENB`/`PARODD`。但 Linux pty 驱动会把 `CSIZE`
  归一化为 `CS8` 并清掉校验位（`tcsetattr` 成功、回读仍是 8N），所以 PTY 后端的
  噪声/掩码实际按 8N 建模；这是 PTY 的固有限制，已在单测中锁定。
- **只处理真正入队的数据**：噪声/掩码在内核写路径中只作用于对端环形缓冲能接收的
  字节，避免“被流控丢弃的字节也被计为损坏”。

## 有意保留的差异与未实现项

| 项目 | 上游 | 本项目 | 原因 |
|---|---|---|---|
| 引脚逻辑极性 | 每个目标引脚可配置正/负逻辑（`C0C_PIN_*_NEGATIVE`） | 只支持“输出置位 → 输入置位” | 保持 4 字节 wire 格式不扩张；默认接线已等价 |
| 本地回环接线 | 支持 `L*`（本端输出驱动本端输入）与 `OPEN` 伪引脚 | 不支持 | 使用场景少，需新增 wire 字段 |
| 握手门控 | RTS/CTS、DTR/DSR、DCD 握手真实阻塞读写 | `handflow` 只存储与回读，不阻塞数据 | 现有 SDK 能力位未声明；避免半实现 |
| `GET_COMMSTATUS.Errors` | 汇总 LSR 错误位，`SERIAL_ERROR_ABORT` 可中止 IOCTL | 恒为 0 | 错误已进 `SERIALPERF_STATS` 与 `SERIAL_EV_ERR`，LSR 汇总待补 |
| 特殊字符 | `AddRTTO`/`AddRITO`、`SERIAL_LSRMST_INSERT` 会改写数据流 | 控制码已识别，行为为无操作 | 需要逐字节扫描，影响零拷贝路径 |
| XOFF 超时 | `XOFF_COUNTER` 的 `Timeout`/`Usable` 参与定时 | 只按 `Counter` 计数 | 无真实线路时钟 |
| 私有扩展 IOCTL | `C0CE_*` 签名、`GET_MODEM_CONTROL` 扩展缓冲 | 只实现标准语义 | 避免绑定上游私有 ABI |
| 配置持久化 | 注册表 `HKLM\...\com0com\Parameters` | SDK/`com0comd` 状态文件（schema 2） | 跨平台统一；Windows 注册表持久化仍在待办 |
| 安装/驱动包 | INF、`setupc` 安装服务、WMI | 仅构建驱动，安装由 VM 测试脚本完成 | 发行工程不属于本次对齐范围 |

## 兼容性注意事项

- **噪声标度变更**：`noise_probability` 从 0.01% 单位改为 1e8 分母。快照 schema 1
  文件在加载时自动乘 10_000 并封顶 1e8（`state_io::decode_snapshot`）；C ABI 结构体
  末尾新增 `all_data_bits`，`C0C_SIZEOF_C0C_PARAMS` 由 96 变为 100，
  `C0C_SIZEOF_C0C_PAIR_INFO` 由 100 变为 104，重新编译调用方即可。
- **wire 协议**：`Com0comPairParams` 变为 164 字节（新增 `all_data_bits`），
  总线与端口驱动必须同版本加载；布局测试锁定了各字段偏移。
- **默认接线**：`Com0comPairParams::new()`/`Default` 现在把 `pin_wiring` 初始化为
  `DEFAULT_PIN_WIRING`（RTS→CTS、DTR→DSR 且 DTR→DCD、OUT1/OUT2 悬空）。端口驱动
  完全按这四个字节算 MSR，若沿用全零默认，建对后用户态还没下发参数时 CTS/DSR 会恒为低。
  C ABI 同理：`params_from_c()` 把全零接线当作"未指定"填成 `NULL_MODEM`，兼容
  `c0c_params p = {0}` 的清零写法（`docs/c-abi.md` 有说明）。Rust 模型与 JSON 快照
  仍可显式表达 `[0,0,0,0]` 的全悬空接线。
- **内核侧验证状态**：`com0comport`（`serial.rs`/`readwrite.rs`）已用本机 WDK 10.0.28000 +
  Windows SDK 10.0.26100 头文件 + libclang 18.1.1 真实编译通过（`cargo build -j 2
  --manifest-path crates/com0comport/Cargo.toml`，0 warning / 0 error），产物
  `crates/com0comport/target/debug/com0comport.dll`；`com0combus` 本轮只改了注释，未重编译。
  **仍未在测试虚拟机加载驱动**跑引脚/噪声端到端用例，合入前需按 `tools/vm-test/` 补验。
  内核工作区的构建前置是 gitignore 的 `target/sdk26100` junction 与 `target/libclang18`
  wheel，清过 `target/` 后要按 `.env` 里的两行命令重建。
