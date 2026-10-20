# SBOM（软件物料清单）

## 目标
为 `com0combus.sys`、`com0comport.sys`、`setupc.exe` 生成 SBOM，纳入 OpenSSF 供应链透明要求。

## 来源
- Rust 依赖：由 `cargo metadata` / `cargo cyclonedx` 生成（CycloneDX/SPDX）。
- 系统/构建依赖：WDK、Clang/LLVM、Windows SDK 版本须记录。

## 计划
1. CI 中安装 `cargo-cyclonedx`。
2. 对每个 crate 生成 CycloneDX JSON。
3. 与签名产物一并归档发布。

> 本机无法构建，SBOM 生成须在 eWDK/CI 环境完成。
