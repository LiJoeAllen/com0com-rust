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
