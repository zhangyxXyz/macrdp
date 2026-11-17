# Stable RSA adapters / RSA 稳定版适配

The pinned IronRDP revision uses picky `7.0.0-rc.25`, sspi `0.21.3` and
winscard `0.3.3`. Each published manifest requires exactly `rsa 0.10.0-rc.18`.
The [RustSec Marvin advisory](https://rustsec.org/advisories/RUSTSEC-2023-0071.html)
lists `rsa >= 0.10.0` as patched. Updating only the lockfile cannot satisfy those
exact prerelease constraints.

The three local packages keep their existing API and package versions while
adapting to the **official crates.io RSA 0.10.0 implementation**. They are local
compatibility changes, not newly published upstream releases. RSA itself is not
vendored, relabeled or modified. This avoids replacing the RDP protocol stack to
fix a transitive authentication dependency.

当前 IronRDP 使用的 picky、sspi、winscard 都精确依赖 RSA 候选版，仅更新锁文件
无法选中 RustSec 列为已修复的稳定版。本地三个适配包保留原有对外 API 和包版本，
对接 crates.io 官方 RSA `0.10.0`，不复制、改写或伪装 RSA 实现，也不因此更换 RDP
协议栈。各目录 `CLAUDE.md` 记录原始版本、提交、归档校验值和实际偏差；许可证保留。

Picky also carries two narrow upstream fixes: [#557](https://github.com/Devolutions/picky-rs/pull/557)
normalizes leading-zero EC components to their curve width, and
[#563](https://github.com/Devolutions/picky-rs/pull/563) unifies RSA JWE unwrap
failure handling with a random fallback key followed by authenticated decryption.
The EC defect reproduced deterministically with an existing P-521 fixture in
the unpatched source; it was not dismissed as a flaky test.

另回移 Picky 的 EC 字段前导零规范化修复 #557 和 RSA JWE 解密失败统一处理 #563。
前者已用既有 P-521 固定样本在旧源码稳定复现，没有通过重跑或跳过测试掩盖。
后者用随机备用密钥继续认证解密，不允许未认证明文返回。

## Resolution boundaries / 依赖解析边界

The root manifest and the independently built `ironrdp-server` and
`ironrdp-acceptor` manifests must all patch picky/sspi/winscard to the same local
directories. Cargo ignores a dependency's own `[patch]` table: a root-only patch
would leave standalone builds using the published prerelease constraints.
The standalone RDPEUDP and IFD packages do not currently depend on RSA.

根项目、独立 server 和独立 acceptor 都必须选择同一组适配目录。Cargo 不继承依赖包
自己的 `[patch]`；不能只修根目录而漏掉独立构建。RDPEUDP 与 IFD 当前不依赖 RSA。

The scheduled security workflow audits the root, standalone server/acceptor, RDPEUDP and
IFD graphs. The root uses its committed lockfile; standalone vendor locks are
not committed and resolve under their own manifest patches. Authentication
regressions must include a real CredSSP exchange: the in-memory connection tests
disable CredSSP and cannot establish authentication compatibility by themselves.

安全工作流检查根依赖树、独立 server／acceptor、RDPEUDP 与 IFD。根目录使用已提交锁文件，
vendor 独立锁文件不提交、按各自 manifest 补丁解析。认证验证须包含真实 CredSSP
交换；内存连接测试禁用了 CredSSP，不能单独证明认证兼容。

## Maintenance / 后续维护

Remove the adapters once compatible published authentication dependencies accept
patched stable RSA. Check both the root and fresh standalone resolution, rerun
the RSA API regressions and good/bad-password FreeRDP smoke, and verify no
affected RSA version has returned before removing these patches. Do not restore
the old advisory ignore to make an incompatible upgrade pass.

待兼容的上游认证依赖正式采用已修复 RSA 后移除适配。必须检查根目录和全新独立解析，
重跑 RSA API 回归及正确／错误密码的 FreeRDP 冒烟，确认旧 RSA 没有重新进入依赖树；
不能通过恢复漏洞忽略项使不兼容升级通过。
