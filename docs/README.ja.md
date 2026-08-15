<p align="center">
  <img src="../apps/desktop/src-tauri/icons/wakegpt-icon.svg" width="112" alt="WakeGPT アイコン">
</p>

<h1 align="center">WakeGPT</h1>

<p align="center"><strong>今すぐ書き留め、自分のワークスペースで育てる。</strong></p>

<p align="center">テキスト、Markdown、リンク、画像を継続的なワークスペースノートに変える、ローカルファーストのデスクトップメモツールです。</p>

<p align="center">
  <a href="../README.md">English</a> ·
  <a href="README.zh-CN.md">简体中文</a> ·
  日本語 ·
  <a href="README.fr.md">Français</a> ·
  <a href="README.ru.md">Русский</a>
</p>

---

## ひらめきを、育ち続けるノートへ

短いメモを書き、ワークスペースとノートブックを選ぶだけで、WakeGPT はその内容を Markdown ファイルへ継続的に追記できます。フォルダーを整理したり、エディターを開いたり、作業中のアプリを離れたりする必要はありません。まず記録し、あとで整えられます。

こんな場面に向いています。

- 開発中のタスク、エラーの手掛かり、実装アイデアをすばやく残す。
- 製品検討や AI との会話から、結論、リンク、スクリーンショットを集める。
- 小さな着想を一つの Markdown ノートへ積み重ねる。
- 過去のメモを選び、ChatGPT の入力欄で再利用する。

## WakeGPT でできること

- **すばやく記録** — プレーンテキスト、Markdown、リンク、画像に対応し、クリップボード画像も直接貼り付けられます。
- **Markdown へ継続追記** — ノートブックを新規作成するか、ワークスペース内の既存 `.md` ファイルを関連付けて、新しい記録を自動追記します。
- **複数のワークスペースとノートブック** — プロジェクトごとに分け、受信トレイと各ノートブックをタブで切り替えます。
- **記録形式を選択** — 連番、箇条書き、タスクリスト、時刻プレフィックスを選び、変更後は自動で並べ直せます。
- **記録の管理** — 閲覧、編集、コピー、移動、削除、復元に対応。画像はサムネイル表示と拡大表示ができます。
- **ChatGPT へ渡す** — 選択した文章と実際の画像添付を ChatGPT の現在の入力欄へ置き、送信前に確認できます。
- **ローカルファースト** — メモ、画像、ワークスペースファイルは既定で端末内に残り、主要機能はテレメトリーに依存しません。

## 3 つの記録場所

| 場所 | 主な用途 |
|---|---|
| **WakeGPT デスクトップアプリ** | ワークスペース、ノートブック、履歴、設定をまとめて管理します。 |
| **macOS メニューバー** | 作業を中断せず、軽量な入力パネルを開きます。 |
| **ChatGPT サイドカード** | 会話の横で記録し、ノートブックを切り替え、選択した内容を ChatGPT の入力欄へ置きます。 |

デスクトップアプリとクイックパネルは、現在のノートブックと下書きをそれぞれ保持します。一方を切り替えても、もう一方の表示が勝手に変わることはありません。

## 記録の流れ

1. テキスト、Markdown、リンクを入力するか、画像を貼り付けます。
2. ワークスペースの受信トレイまたは Markdown ノートブックを選びます。
3. 送信するとローカルへ保存され、関連付けたノートブックでは Markdown ファイルにも追記されます。
4. あとから編集、移動、削除、復元したり、ChatGPT で再利用したりできます。

WakeGPT が管理するのは、Markdown 内で明示的にマークされたクイックノート領域だけです。文書のほかの部分は、好きなエディターで引き続き編集できます。

## 現在の状態

WakeGPT は現在、オープンソースの **macOS プロトタイプ**です。[`v0.1.1` macOS 正式 GitHub Release](https://github.com/Awaker-OTE/WakeGPT/releases/tag/v0.1.1)をダウンロードできます。

- Apple Silicon macOS で動作を確認済みです。
- `v0.1.1` は macOS 専用の正式な GitHub Release です。
- Universal App Bundle には Apple Silicon と Intel の両方のコードが含まれますが、実機の Intel Mac では未確認です。
- このリリースは hardened runtime を有効にした完全な ad-hoc コードシールを使用しますが、Apple Developer ID では署名されず、Apple の公証も受けていません。そのため macOS が初回起動を止める場合があります。
- ChatGPT サイドカードは実験的機能で、リポジトリに記録された特定のホスト版でのみ検証済みです。ホスト契約が変わった場合は安全に無効化されます。
- Windows x64 版は開発中で、正式リリースおよび Windows 実機での検証はまだ行われていません。

必ず[公式の `v0.1.1` GitHub Release](https://github.com/Awaker-OTE/WakeGPT/releases/tag/v0.1.1)からダウンロードし、同梱の `SHA256SUMS.txt` で SHA-256 を照合してください。WakeGPT を一度開こうとした後、macOS に止められた場合は **システム設定 → プライバシーとセキュリティ → このまま開く** を選びます。[Apple の公式手順](https://support.apple.com/ja-jp/guide/mac-help/mh40616/mac)も参照してください。WakeGPT は Gatekeeper の無効化や `xattr` による隔離属性の削除を求めません。

## ソースから実行

Node.js、npm、Rustup、および [Tauri 2 のシステム要件](https://v2.tauri.app/start/prerequisites/)が必要です。

```bash
git clone https://github.com/Awaker-OTE/WakeGPT.git
cd WakeGPT/apps/desktop
npm ci
npm test
npm run tauri build
```

macOS Universal ビルド：

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
npm run release:macos-universal
```

ソースから生成したものは引き続き開発用またはローカル検証用です。ダウンロード可能な `v0.1.1` も Apple 公証は受けていませんが、WakeGPT のリリース検査を通し、チェックサムとサプライチェーン記録を添えて公開しています。

## プライバシーと安全性

- メモ、添付ファイル、ワークスペースの Markdown は既定でローカルに保存され、テレメトリーとクラッシュ送信は既定で無効です。
- ChatGPT 連携は入力欄の内容を準備するだけで、送信ボタンを代わりに押すことはありません。
- WakeGPT は ChatGPT のパスワード、Cookie、ログイントークンを読み取ったり保存したりしません。
- 削除では WakeGPT の復元可能なごみ箱または macOS のゴミ箱を優先し、ファイル競合時は上書きせず安全に停止します。

セキュリティ上の問題は [SECURITY.md](../SECURITY.md) の手順に従い、非公開で報告してください。公開 Issue に脆弱性の詳細、アカウント情報、実ユーザーデータを掲載しないでください。

## コントリビューション

バグ報告、機能提案、ドキュメント改善、コード貢献を歓迎します。始める前に [CONTRIBUTING.md](../CONTRIBUTING.md) をお読みください。

WakeGPT は [Apache License 2.0](../LICENSE) で公開され、著作権表示は `Copyright 2026 WakeGPT Contributors` です。第三者コンポーネントには各ライセンスが適用され、出所とライセンス概要は [NOTICE.md](../NOTICE.md) に記載しています。

> WakeGPT は独立して設計・実装されたオープンソースプロジェクトです。OpenAI、ChatGPT、Codex の公式製品ではなく、承認や互換性保証を示すものでもありません。
