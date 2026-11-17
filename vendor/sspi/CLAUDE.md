# RSA stable compatibility / RSA 稳定版兼容补丁

Source: crates.io `sspi 0.21.3` (published source copied without Cargo cache metadata or its standalone lockfile).
Upstream commit: `09088ac49cf13449656dca94b68f5228919a4d95`; repository path: ``.
Crates.io archive SHA-256: `5c3bd2a45e7e24fd72eba100ced3fd847e05fe4657d7f067eb06d1a894f0d4e5`.
Upstream license files are retained. Package version is unchanged; this is a local compatibility patch, not an upstream release.

## Local changes

- Require official `rsa =0.10.0`; do not modify or relabel the RSA implementation. Its Rust 1.87 minimum is satisfied by this repository's Rust 1.95 toolchain.
- Align the macOS/iOS direct PKCS#1 dependency to 0.8.0. No SSPI authentication or protocol implementation changes.
- Keep normalized `Cargo.toml` and original workspace manifest `Cargo.toml.orig` dependency intent synchronized.

中文：只对接官方 RSA 0.10.0，保留上游包版本与许可证，不复制或修改密码学算法。
根项目通过 `[patch.crates-io]` 选择本目录；更新上游依赖时应重新核对并尽早移除这些兼容补丁。
