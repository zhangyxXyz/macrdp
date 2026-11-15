# Changelog / 更新记录

Only implemented behavior and completed checks are recorded here. Client-side manual checks are identified separately.
这里只记录已实现的行为和已完成的检查；客户端实测另行注明。

## v0.10.5-dev

### English

- Fix Windows screenshot clipboard selection: try registered PNG, DIBV5, then DIB before the virtual-file representation. Successful images skip the redundant file request.
- For top-level image-only virtual-file copies, download and decode the complete batch before publishing real file URLs and PNG data. Do not expose sparse placeholders to apps that read files directly. Reject corrupt/incomplete images and stale copies; complete short range responses instead of leaving zero-filled tails.
- Bound this image-file path to 16 files, 50 MiB total source and normalized data, 16 megapixels per image, 8192 pixels per side, and a 120-second batch timeout. Other files and mixed/folder copies retain their existing transfer path. Animated formats provide a decoded still image for image paste; the downloaded file retains its original bytes.
- Include earlier fork work: validated/atomic configuration, display policy and measured state, reconnect lifecycle handling, and independent Win/Alt position swapping and Ctrl editing compatibility.
- Package only on `v*` tag pushes. Upload directly to a draft Release, download and verify checksum/source/bundle contents, then publish. Retain the newest verified pipeline-owned Release and preserve historical Git tags. Ordinary dev/PR checks remain.
- Automated regression tests cover image corruption, format fallback, complete file publication, short reads, stale/local clipboard changes and release verification/cleanup. Real Windows screenshot-copy validation of this version is pending user testing. Existing local native 4K/200% display results are documented in the README.

### 简体中文

- 修复 Windows 截图剪贴板格式选择：依次尝试注册的 PNG、DIBV5、DIB，再回退到虚拟文件；图片成功后跳过多余的文件请求。
- 顶层纯图片文件批次先完整下载、解码校验，再发布真实文件 URL 和 PNG 数据，避免直接读文件的应用拿到稀疏占位内容。损坏、不完整或已过期的复制不发布；文件分块短读继续补齐，避免留下全零尾部。
- 该图片文件路径限制为最多 16 个文件、源数据和标准化数据各合计 50 MiB、单图 1600 万级像素（16×1024×1024）、单边 8192 像素、整批 120 秒超时。普通文件、混合批次和文件夹保持现有传输路径。动图的图片粘贴提供解码后的静态画面，下载文件保留原始字节。
- 包含此前 fork 开发：配置校验与原子保存、显示策略与真实状态、重连生命周期处理，以及独立的 Win／Alt 位置交换和 Ctrl 编辑兼容设置。
- 仅推送 `v*` tag 时打包；直接上传草稿 Release，回读核对校验和、源提交和包内容后发布。默认仅保留最新已验证、由本流水线创建的 Release，历史 Git tags 保留；dev／PR 的日常检查继续执行。
- 自动回归覆盖图片损坏、格式回退、完整文件发布、短读、过期／本机剪贴板变化，以及发布校验和清理。本版 Windows 截图复制仍待用户实测；此前本机原生 4K／200% 的验证记录见 README。

Thanks / 致谢：感谢 [clintcan/macrdp](https://github.com/clintcan/macrdp) 与 IronRDP 的贡献者；发布清理编排参考 [CLIProxyAPI-Plugins](https://github.com/zhangyxXyz/CLIProxyAPI-Plugins)。
