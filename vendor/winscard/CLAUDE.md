# RSA stable compatibility / RSA 稳定版兼容补丁

Source: crates.io `winscard 0.3.3` (published source copied without Cargo cache metadata or its standalone lockfile).
Upstream commit: `fb5ce085e48c337b03968349f5e2e85c83959754`; repository path: `crates/winscard`.
Crates.io archive SHA-256: `12dafb3c1468d0a3f5440e21e51614b53d1fdc62c9f82cc861c447906d09c69a`.
The crate archive omitted license files; `LICENSE-MIT` and `LICENSE-APACHE` were copied from the upstream repository root at the commit above (byte-identical to the sspi 0.21.3 archive licenses). Package version is unchanged; this is a local compatibility patch, not an upstream release.

## Local changes

- Require official `rsa =0.10.0`; do not modify or relabel the RSA implementation. Its Rust 1.87 minimum is satisfied by this repository's Rust 1.95 toolchain.
- Preserve the upstream package's external API; enable RSA `pkcs1v15` and `getrandom` features explicitly.
- Blind the already-padded PIV private-key operation with `getrandom::SysRng` (OS CSPRNG), retaining RSA's public-key fault check and avoiding double padding. Remove the unused dummy RNG.
- Test blinded PIV signatures, out-of-range rejection, and SHA-1 prehashed signing.
- Keep normalized `Cargo.toml` and original workspace manifest `Cargo.toml.orig` dependency intent synchronized.

中文：只对接官方 RSA 0.10.0，保留上游包版本与许可证，不复制或修改密码学算法。
根项目通过 `[patch.crates-io]` 选择本目录；更新上游依赖时应重新核对并尽早移除这些兼容补丁。
