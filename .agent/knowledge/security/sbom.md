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

> 生成 SBOM 需在装有 WDK 的构建机 / CI 环境完成；WDK、Windows SDK、libclang
> 版本已记录在仓库根 `.env`，可直接作为系统构建依赖的输入。
