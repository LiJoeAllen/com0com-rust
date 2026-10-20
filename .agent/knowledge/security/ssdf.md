# SSDF 映射（OpenSSF Secure Software Development Framework）

| 实践 | 本项目落地 |
|------|-----------|
| PO（保护软件）: 安全需求 | 见 `openspec/specs/`、`brain/brain/risks.md` 记录安全需求 |
| PS（生产良好软件）: 依赖管理 | 锁定 `cargo` 依赖版本；Rust 生态 + `wdk`/`windows-sys` 版本固定 |
| PS: 安全编译 | 内核用 Clang/LLVM（WDK 默认）；`panic=abort`，无 unwind |
| PW（交付良好软件）: 漏洞披露 | 项目 README/SECURITY 提供反馈渠道（待补） |
| RV（验证）: 漏洞扫描 | 依赖审计 `cargo audit`；SBOM（见 `sbom.md`） |
| ID（识别）: 配置/密钥 | 无硬编码密钥；签名证书走 CI 机密，不入库 |

> 内核签名链、HLK 测试等属交付阶段，须在 eWDK/测试 VM 完成。

## cargo audit 已知接受的“非漏洞”信息项（informational）

| Advisory | Crate | 类型 | 决策 |
|----------|-------|------|------|
| RUSTSEC-2024-0436 | `paste` 1.0.15 | unmaintained（未维护） | **接受**：wdk 工具链传递依赖，无补丁版本；仅影响 proc-macro 展开，不影响驱动运行时安全。定期复审。 |

> `cargo audit` 结果显示 `vulnerabilities: 0`；上述仅为 informational 警告，不阻断 CI。
> 复审方式：`cargo audit`（CI job `audit`），若上游 wdk 生态迁移到 `pastey` 则随版本更新消除。
