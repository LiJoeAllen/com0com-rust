# security/ — OpenSSF 安全与供应链

本项目为 Windows 内核驱动，安全与供应链要求是头等大事。本目录落实 **OpenSSF**（Open Source Security Foundation）相关实践。

## 采用清单
- **SSDF（Secure Software Development Framework）**：见 `ssdf.md`——把身份鉴别、依赖管理、漏洞披露映射到内核驱动开发。
- **Scorecard**：见 `scorecard.md`——跟踪仓库风险打分（分支保护、签名、依赖、令牌权限等）。
- **SBOM（软件物料清单）**：见 `sbom.md`——驱动与 setupc 的依赖与构建产物清单。

## 本项目特有关注点
1. 内核驱动必须经 EV 证书签名 + HLK/Attestation 签名才能加载（测试可用测试签名/`TESTSIGNING`）。
2. 用户态 `setupc` 需管理员权限打开总线接口——最小权限原则。
3. 私有 `IOCTL_COM0COM_*` 是攻击面——须做输入校验、长度边界、权限检查。
4. `PairContext` 跨端口裸指针共享——须保证引用计数与生命周期正确，防 UAF。
