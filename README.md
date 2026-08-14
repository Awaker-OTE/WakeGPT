<p align="center">
  <img src="apps/desktop/src-tauri/icons/wakegpt-icon.svg" width="112" alt="WakeGPT icon">
</p>

<h1 align="center">WakeGPT</h1>

<p align="center">
  <strong>Capture it now. Keep it growing in your own workspace.</strong>
</p>

<p align="center">
  A local-first desktop scratchpad for turning text, Markdown, links, and images into durable workspace notes.
</p>

<p align="center">
  <strong>Capture → Keep in Markdown → Reuse in ChatGPT</strong>
</p>

<p align="center">
  <a href="https://github.com/Awaker-OTE/WakeGPT/releases/tag/v0.1.0"><strong>Download for macOS</strong></a> ·
  <a href="#what-wakegpt-does">See features</a> ·
  <a href="#project-status">Project status</a> ·
  <a href="CONTRIBUTING.md">Contribute</a>
</p>

<p align="center">
  <sub>v0.1.0 prerelease · Verified on Apple silicon macOS · Not notarized by Apple</sub>
</p>

<p align="center">
  English ·
  <a href="docs/README.zh-CN.md">简体中文</a> ·
  <a href="docs/README.ja.md">日本語</a> ·
  <a href="docs/README.fr.md">Français</a> ·
  <a href="docs/README.ru.md">Русский</a>
</p>

---

## Turn passing thoughts into notes that last

Write a quick entry, choose a workspace and notebook, and WakeGPT can keep appending it to your Markdown file. You do not need to organize folders, open an editor, or leave the task at hand. Capture the thought first and refine it later.

WakeGPT is useful when you want to:

- capture todos, error clues, and implementation ideas while developing;
- collect conclusions, links, and screenshots during product work or AI conversations;
- grow one Markdown note from many small entries;
- reuse selected notes in a ChatGPT prompt without retyping them.

## What WakeGPT does

- **Fast capture** — Write plain text, Markdown, and links, or import and paste images directly.
- **Continuous Markdown notes** — Create a notebook or bind an existing `.md` file in a workspace, then append new entries automatically.
- **Multiple workspaces and notebooks** — Keep projects separate and switch between an inbox and notebook tabs.
- **Flexible entry styles** — Use sequential numbers, bullets, task lists, or time prefixes, with automatic reordering after changes.
- **Record management** — View, edit, copy, move, delete, and restore entries. Images appear as thumbnails and open in a larger preview.
- **Send to ChatGPT** — Put selected text and the real image attachments into the current ChatGPT composer for review before sending.
- **Local first** — Notes, images, and workspace files stay on your computer by default. Core features do not depend on telemetry.

## Capture from three places

| Surface | Best for |
|---|---|
| **WakeGPT desktop app** | Managing workspaces, notebooks, history, and settings in one place. |
| **macOS menu bar** | Opening a lightweight capture panel without leaving your current work. |
| **ChatGPT side card** | Taking notes beside a conversation, switching notebooks, and placing selected entries into the ChatGPT composer. |

The desktop app and quick panels keep their own current notebook and draft. Switching in one surface does not unexpectedly move another surface away from what you were viewing.

## How an entry flows

1. Enter text, Markdown, or a link, or paste an image.
2. Choose the workspace inbox or a Markdown notebook.
3. Submit once to save locally; when a notebook is bound, WakeGPT also appends the entry to its Markdown file.
4. Return later to edit, move, delete, restore, or reuse the entry in ChatGPT.

WakeGPT manages only clearly marked quick-note regions in a Markdown file. The rest of the document remains yours to edit with any tool.

## Project status

WakeGPT is open source and currently available as a **macOS prototype**. The [`v0.1.0` macOS prerelease](https://github.com/Awaker-OTE/WakeGPT/releases/tag/v0.1.0) is available now.

- Runtime behavior has been verified on Apple Silicon macOS.
- `v0.1.0` is a macOS-only prerelease.
- Its Universal app bundle contains both Apple Silicon and Intel code; execution on physical Intel hardware is still unverified.
- The release uses complete ad-hoc code seals and hardened runtime, but it is **not** signed with an Apple Developer ID or notarized by Apple. macOS may therefore block the first launch.
- The ChatGPT side card is experimental and has been verified only against the exact host versions recorded by this repository. It safely disables itself when the host contract changes.
- Windows x64 support is in development and is not yet released or verified on physical Windows hardware.

Download `v0.1.0` only from its [official GitHub Release](https://github.com/Awaker-OTE/WakeGPT/releases/tag/v0.1.0) and compare its SHA-256 value with the included `SHA256SUMS.txt`. After trying to open WakeGPT once, macOS may require you to open **System Settings → Privacy & Security** and choose **Open Anyway**. See [Apple's instructions for opening an app from an unknown developer](https://support.apple.com/guide/mac-help/open-a-mac-app-from-an-unknown-developer-mh40616/mac). WakeGPT does not ask users to disable Gatekeeper or remove quarantine attributes with `xattr`.

## Run from source

You will need Node.js, npm, Rustup, and the [Tauri 2 system prerequisites](https://v2.tauri.app/start/prerequisites/).

```bash
git clone https://github.com/Awaker-OTE/WakeGPT.git
cd WakeGPT/apps/desktop
npm ci
npm test
npm run tauri build
```

For a macOS Universal build:

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
npm run release:macos-universal
```

Source builds remain development or local-verification artifacts. The downloadable `v0.1.0` package is also unnotarized, but it was produced through WakeGPT's release checks and is published with checksums and supply-chain records.

## Privacy and safety

- Notes, attachments, and workspace Markdown files remain local by default; telemetry and crash uploads are off by default.
- ChatGPT integration prepares composer content but never clicks Send for you.
- WakeGPT does not read or store ChatGPT passwords, cookies, or login tokens.
- Deletion prefers WakeGPT's recoverable trash or the macOS Trash. File conflicts fail safely instead of overwriting changes.

Report security issues privately as described in [SECURITY.md](SECURITY.md). Do not put vulnerability details, account information, or real user data in a public issue.

## Contributing

Bug reports, feature proposals, documentation improvements, and code contributions are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) before starting.

If WakeGPT helps you keep useful ideas from getting lost, consider starring the repository so you can follow future releases.

WakeGPT is licensed under the [Apache License 2.0](LICENSE), with `Copyright 2026 WakeGPT Contributors`. Third-party components retain their own licenses; provenance and license summaries are listed in [NOTICE.md](NOTICE.md).

> WakeGPT is an independently designed and implemented open-source project. It is not an official OpenAI, ChatGPT, or Codex product, and no endorsement or compatibility guarantee is implied.
