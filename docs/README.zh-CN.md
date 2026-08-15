<p align="center">
  <img src="../apps/desktop/src-tauri/icons/wakegpt-icon.svg" width="112" alt="WakeGPT 图标">
</p>

<h1 align="center">WakeGPT</h1>

<p align="center"><strong>随手记下，持续写进自己的工作区。</strong></p>

<p align="center">一款本地优先的桌面速记工具，让文字、Markdown、链接和图片变成可持续积累的工作区笔记。</p>

<p align="center">
  <a href="../README.md">English</a> ·
  简体中文 ·
  <a href="README.ja.md">日本語</a> ·
  <a href="README.fr.md">Français</a> ·
  <a href="README.ru.md">Русский</a>
</p>

---

## 把一闪而过的想法，变成持续生长的笔记

写下一条记录，选择工作区和速记本，WakeGPT 就能把它持续追加到你的 Markdown 文件中。你不需要先整理目录、打开编辑器或切换应用；先把内容留下，稍后再统一整理。

它适合这些时刻：

- 在开发过程中快速记下待办、错误线索和实现想法；
- 在产品讨论或 AI 对话中收集结论、链接与截图；
- 把零散灵感持续沉淀到同一份 Markdown 笔记；
- 从历史记录中挑选内容，放进 ChatGPT 输入框继续讨论。

## WakeGPT 能做什么

- **快速记录**：支持纯文本、Markdown、链接和图片，也可以直接粘贴剪贴板图片。
- **持续写入 Markdown**：新建速记本，或绑定工作区中已有的 `.md` 文件，后续记录自动追加。
- **多个工作区与速记本**：按项目管理不同记录，通过 Tab 在收件箱和多个速记本之间切换。
- **自动编号与整理**：可选择连续数字、项目符号、任务列表、时间前缀等记录样式，并在变化后自动重排。
- **完整记录管理**：查看、编辑、复制、迁移、删除和恢复记录；图片支持缩略图与放大预览。
- **发送到 ChatGPT 输入框**：把选中的文字与真实图片附加到当前输入框，由你确认后再发送。
- **本地优先**：记录、图片和工作区文件默认留在本机；核心功能不依赖遥测。

## 三个入口，随时记录

| 入口 | 最适合做什么 |
|---|---|
| **WakeGPT 主应用** | 管理工作区、速记本、历史记录和设置，集中整理长期笔记。 |
| **macOS 菜单栏** | 不打断当前工作，随时打开轻量输入面板。 |
| **ChatGPT 侧边速记卡** | 在对话旁边记录内容、切换速记本，并把选中记录放入 ChatGPT 输入框。 |

主应用与快速面板各自保留当前选择和草稿；切换其中一个，不会强行改变另一个正在查看的速记本。

## 一条记录如何流转

1. 输入文字、Markdown、链接，或粘贴图片。
2. 选择工作区中的收件箱或 Markdown 速记本。
3. 提交后立即保存在本地；绑定速记本时，同步追加到对应 Markdown 文件。
4. 随时回来编辑、移动、删除、恢复，或把内容放入 ChatGPT 输入框继续使用。

WakeGPT 只管理 Markdown 中带有明确标记的速记区域，不会接管整份文件。你仍然可以用喜欢的编辑器处理其他正文。

## 当前状态

WakeGPT 目前是开源的 **macOS 原型**，[`v0.1.1` macOS 预发布版](https://github.com/Awaker-OTE/WakeGPT/releases/tag/v0.1.1)现已可以下载。

- 已在 Apple Silicon macOS 上完成运行验证。
- `v0.1.1` 是仅面向 macOS 的预发布版。
- 其中的 Universal App Bundle 同时包含 Apple Silicon 与 Intel 代码；真实 Intel Mac 运行仍待验证。
- 此版本采用完整的 ad-hoc 代码签封并启用 hardened runtime，但**没有**使用 Apple Developer ID 签名，也**没有**经过 Apple 公证，因此 macOS 可能阻止首次启动。
- ChatGPT 侧边速记卡属于实验性接入，目前只对仓库记录的精确宿主版本完成验证；宿主契约变化时会安全停用。
- Windows x64 版本正在开发中，尚未正式发布，也未在真实 Windows 硬件上完成验证。

请只从 [`v0.1.1` 官方 GitHub Release](https://github.com/Awaker-OTE/WakeGPT/releases/tag/v0.1.1) 下载，并用其中的 `SHA256SUMS.txt` 核对 SHA-256。首次尝试打开 WakeGPT 后，如 macOS 拦截，请进入 **系统设置 → 隐私与安全 → 仍要打开**。可参考 [Apple 官方说明](https://support.apple.com/zh-cn/guide/mac-help/mh40616/mac)。WakeGPT 不会要求用户关闭 Gatekeeper，也不会要求执行 `xattr` 来移除隔离属性。

## 从源码运行

需要 Node.js、npm、Rustup，以及 [Tauri 2 的系统依赖](https://v2.tauri.app/start/prerequisites/)。

```bash
git clone https://github.com/Awaker-OTE/WakeGPT.git
cd WakeGPT/apps/desktop
npm ci
npm test
npm run tauri build
```

macOS Universal 构建入口：

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
npm run release:macos-universal
```

源码构建仍属于开发或本机验证产物。可下载的 `v0.1.1` 同样未经 Apple 公证，但已经过 WakeGPT 的发布检查，并同时提供校验和与供应链记录。

## 隐私与安全

- 记录、附件和工作区 Markdown 默认保存在本机，遥测与崩溃上传默认关闭。
- ChatGPT 接入只准备输入框内容，不会替你点击发送。
- WakeGPT 不读取或保存 ChatGPT 密码、Cookie 或登录令牌。
- 删除操作优先进入 WakeGPT 的可恢复回收站或 macOS 废纸篓；发生文件冲突时会安全停用，不会强行覆盖。

如发现安全问题，请按照 [SECURITY.md](../SECURITY.md) 私下报告。不要在公开 Issue 中附上漏洞细节、账号信息或真实用户数据。

## 参与项目

欢迎提交 Bug、功能建议、文档改进和代码贡献。开始前请阅读 [CONTRIBUTING.md](../CONTRIBUTING.md)。

WakeGPT 使用 [Apache License 2.0](../LICENSE) 开源，版权声明为 `Copyright 2026 WakeGPT Contributors`。第三方组件继续适用各自许可证，来源与许可摘要见 [NOTICE.md](../NOTICE.md)。

> WakeGPT 是独立设计和实现的开源项目，不是 OpenAI、ChatGPT 或 Codex 的官方产品，也不代表获得了认可或兼容承诺。
