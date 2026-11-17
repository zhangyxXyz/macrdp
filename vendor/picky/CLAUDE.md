# RSA stable compatibility and upstream fixes / RSA 稳定版兼容与上游修复

Source: crates.io `picky 7.0.0-rc.25` (published source copied without Cargo cache metadata or its standalone lockfile).
Upstream commit: `18a3a419adcbe0034f7fdfc694039cc970d53766`; repository path: `picky`.
Crates.io archive SHA-256: `c1ae9cd78eb1d61be4790713d28368cf71c844218fcd91542768805423f02666`.
Upstream license files are retained. Package version is unchanged; this is a local compatibility patch, not an upstream release.

## Local changes

- Require official `rsa =0.10.0`; do not modify or relabel the RSA implementation. Its Rust 1.87 minimum is satisfied by this repository's Rust 1.95 toolchain.
- Enable RSA `pkcs1v15`, align direct PKCS#1 dependency to 0.8.0, and use stable key names.
- Adapt private-prime export/SSH encoding to `Odd<BoxedUint>` and public-key validation to `new_with_size_limits`, retaining the official 1024-bit minimum and the existing 8192-bit maximum.
- Zeroize the temporary owned prime-export buffer, including error paths.
- Unlike the old prerelease, public keys smaller than 1024 bits are rejected by RSA conversion. Regression tests cover 1023/1024 and 8192/8193-bit boundaries; weak-key acceptance is not restored.
- Add RSA1_5/OAEP/OAEP256 round-trip and key/ciphertext/tag tampering tests. No unauthenticated plaintext is returned.
- Keep normalized `Cargo.toml` and original workspace manifest `Cargo.toml.orig` dependency intent synchronized.

## Backported upstream fixes

- [EC leading-zero fix #557](https://github.com/Devolutions/picky-rs/commit/122303e05e692b083e01834805fe5fdab87cdac3): apply all six `picky/src` file changes, including public/private component padding, fixed-width JWK output and input validation, SSH/PuTTY secret preservation, and upstream regression tests. The upstream fixtures are already published in `picky-test-data`; require dev dependency >=0.1.2 rather than copying or modifying the fixture crate.
- [JWE encrypted-key error masking #563](https://github.com/Devolutions/picky-rs/commit/2e5b128850facd91ee8eb3994fe05f2883239faf): apply the complete `jwe.rs` change and upstream tests. RSA1_5/OAEP/OAEP256 invalid encrypted keys use a fresh random CEK and fail with the same AES-GCM error as invalid content. OS-seeded RNG and explicit RSA blinding are retained; CBC-HS key/tag size reporting is corrected (CBC-HS content encryption remains unsupported). The only adaptation to the upstream comment is that this fork now uses patched official RSA 0.10.0; no timing guarantee is inferred from functional tests.
- Compatibility: EC `from_*_components` accepts numeric values requiring leading-zero padding but rejects nonzero values wider than the curve field; JWK wire coordinates must have exactly the field length. JWE callers must handle `JweError::AesGcm` for invalid encrypted keys instead of matching `JweError::Rsa`.

中文：对接官方 RSA 0.10.0，并精确回移上述两项上游修复；保留上游包版本、来源与许可证，不自行设计密码学算法。
根项目通过 `[patch.crates-io]` 选择本目录；更新上游依赖时应重新核对并尽早移除这些兼容补丁。
