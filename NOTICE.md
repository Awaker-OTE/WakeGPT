# WakeGPT Third-Party Notices

WakeGPT
Copyright 2026 WakeGPT Contributors

WakeGPT is licensed under the Apache License, Version 2.0. The complete project license is in
[`LICENSE`](LICENSE). Third-party components retain the licenses and attribution notices listed
below and in the generated release license bundle.

WakeGPT currently uses the following direct runtime, UI, and Markdown dependencies. This notice supplements,
and does not replace, the license files shipped with dependency sources and release artifacts. Key upstream
sources, purposes, and replacement paths are recorded below.

## Direct dependency summary

Resolved versions are locked by `apps/desktop/package-lock.json` and
`apps/desktop/src-tauri/Cargo.lock`.

| Dependency | Resolved version | Upstream | License |
|---|---:|---|---|
| Tauri API / Tauri / Tauri Build | 2.11.1 / 2.11.5 / 2.6.3 | <https://github.com/tauri-apps/tauri> | Apache-2.0 OR MIT |
| React / React DOM | 19.2.8 | <https://github.com/facebook/react> | MIT |
| React Markdown | 10.1.0 | <https://github.com/remarkjs/react-markdown> | MIT |
| remark-gfm | 4.0.1 | <https://github.com/remarkjs/remark-gfm> | MIT |
| Lucide React | 1.26.0 | <https://github.com/lucide-icons/lucide> | ISC; some icons also carry Feather MIT |
| serde / serde_json | 1.0.229 / 1.0.151 | <https://github.com/serde-rs> | MIT OR Apache-2.0 |
| rusqlite | 0.40.1 | <https://github.com/rusqlite/rusqlite> | MIT |
| bundled SQLite | version selected by rusqlite lock graph | <https://sqlite.org/> | Public Domain |
| time | 0.3.54 | <https://github.com/time-rs/time> | MIT OR Apache-2.0 |
| uuid | 1.24.0 | <https://github.com/uuid-rs/uuid> | Apache-2.0 OR MIT |
| sha2 | 0.10.9 | <https://github.com/RustCrypto/hashes> | MIT OR Apache-2.0 |
| Tauri Dialog | 2.7.2 | <https://github.com/tauri-apps/plugins-workspace> | Apache-2.0 OR MIT |
| Tauri Autostart | 2.5.1 | <https://github.com/tauri-apps/plugins-workspace> | Apache-2.0 OR MIT |
| Tauri Updater | 2.10.1 | <https://github.com/tauri-apps/plugins-workspace> | Apache-2.0 OR MIT |
| reqwest / futures-util | 0.13.4 / 0.3.33 | <https://github.com/seanmonstar/reqwest> / <https://github.com/rust-lang/futures-rs> | MIT OR Apache-2.0 |
| base64 / minisign-verify | 0.22.1 / 0.2.5 | <https://github.com/marshallpierce/rust-base64> / <https://github.com/jedisct1/rust-minisign> | MIT OR Apache-2.0 / MIT |
| rustls | 0.23.42 | <https://github.com/rustls/rustls> | Apache-2.0 OR ISC OR MIT |
| libc | 0.2.189 | <https://github.com/rust-lang/libc> | MIT OR Apache-2.0 |
| tungstenite | 0.30.0 | <https://github.com/snapview/tungstenite-rs> | MIT OR Apache-2.0 |
| objc2-foundation | 0.3.2 | <https://github.com/madsmtm/objc2> | MIT |
| objc2-app-kit | 0.3.2 | <https://github.com/madsmtm/objc2> | Zlib OR Apache-2.0 OR MIT |

Build-only direct dependencies are Tauri CLI 2.11.4, Vite 7.3.6,
`@vitejs/plugin-react` 4.7.0, TypeScript 5.8.3 and the React type packages. Their licenses are
recorded in the dependency register. A distributable Release must additionally ship the generated
third-party license texts and SBOM for the complete resolved transitive graph; this source notice
does not replace that generated evidence or legal review.

`npm run release:inventory` generates the current complete lock-graph CycloneDX 1.6 SBOM and a
machine-readable license inventory under ignored `release-output/supply-chain/`. The output omits
dynamic timestamps for byte reproducibility and records the project license as Apache-2.0. Release
publication must regenerate and review these files from
the clean signed tag; generated local files are not tracked source.

`npm run release:third-party-licenses` separately generates a byte-reproducible
`THIRD-PARTY-LICENSES.txt` and a source manifest for all 717 currently locked npm and Cargo
components. npm tarballs are accepted only from the credential-free official registry and are
verified by package-lock SRI and exact archive identity. Cargo archives are read from the local
cache and verified against Cargo.lock. Archive license or notice evidence is retained when present;
reviewed packages that omit license text use full upstream Git revisions and pinned file SHA-256
values. Where a crate omits VCS metadata, the source manifest additionally records either an exact
Git tag or a per-package manifest digest that binds the reviewed revision to that version. The
current graph produces 337 deduplicated texts. The `r-efi`
archives retain their bundled AUTHORS evidence and pair it with the declared canonical SPDX texts.
Canonical texts are pinned to SPDX License List 3.28.0. Unknown expressions, missing required
evidence, unreviewed evidence filenames, source mismatch, stale or no-longer-needed mappings, or an existing
output directory fail closed. The generated bundle is ignored locally and must accompany a
reviewed Release rather than be committed as hand-maintained source.

## Tauri Autostart 2.5.1

- Source: <https://github.com/tauri-apps/plugins-workspace/tree/v2/plugins/autostart>
- License: MIT OR Apache-2.0.
- Purpose: manage the user's per-account system login item through Tauri's supported desktop
  abstraction while keeping the preference disabled by default.
- Replacement path: replace the plugin with reviewed macOS LaunchAgent and Windows startup-item
  adapters behind the existing WakeGPT login-item command contract.
- Runtime backend: `auto-launch` 0.5.0, MIT, from
  <https://github.com/zzzgydi/auto-launch>.

## Tauri Updater 2.10.1

- Source: <https://github.com/tauri-apps/plugins-workspace/tree/v2/plugins/updater>
- License: MIT OR Apache-2.0.
- Purpose: check an HTTPS release endpoint, download a selected immutable release asset and verify
  update packages against the public key embedded in an explicitly configured release build.
  Local unconfigured builds do not make update requests; installation remains a separate explicit
  user action guarded by WakeGPT's backup, health-check and rollback transaction.
- Replacement path: replace the plugin only with a reviewed updater that preserves signed package
  verification, immutable asset URLs, rate limits, explicit installation and failure-closed rules.

## React Markdown 10.1.0

- Source: <https://github.com/remarkjs/react-markdown>
- License: MIT.
- Purpose: parse CommonMark into React elements without injecting generated HTML strings.
- Replacement path: replace the `MarkdownPreview` renderer with another license-compatible parser
  that preserves the same raw-HTML, URL, remote-image and accessibility boundaries.
- Distribution notice: release artifacts include the complete upstream license text through the
  generated `THIRD-PARTY-LICENSES.txt`; this hand-maintained summary does not duplicate it.

## remark-gfm 4.0.1

- Source: <https://github.com/remarkjs/remark-gfm>
- License: MIT.
- Purpose: add GitHub Flavored Markdown tables, task lists, autolinks, strikethrough and footnotes
  to the CommonMark preview pipeline.
- Replacement path: remove the plugin and either accept CommonMark-only previews or replace it with
  another license-compatible GFM syntax-tree extension covered by the same security tests.
- Distribution notice: release artifacts include the complete upstream license text through the
  generated `THIRD-PARTY-LICENSES.txt`; this hand-maintained summary does not duplicate it.

## Lucide React 1.26.0

- Source: <https://github.com/lucide-icons/lucide>
- License: ISC; some icons are derived from Feather and additionally carry the MIT license included
  in the Lucide distribution.
- Purpose: one consistent, accessible outline-icon family for the WakeGPT desktop interface and
  the independently rendered WakeGPT card in ChatGPT.
- Replacement path: replace the imported Lucide React components and the small audited icon-node
  subset in the Codex adapter with another license-compatible icon family using the shared icon
  component contract.

Copyright (c) 2026 Lucide Icons and Contributors

Permission to use, copy, modify, and/or distribute this software for any purpose with or without
fee is hereby granted, provided that the above copyright notice and this permission notice appear
in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS
SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE
AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT,
NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE
OF THIS SOFTWARE.

For Lucide icons derived from the Feather project:

The MIT License (MIT)

Copyright (c) 2013-present Cole Bemis

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and
associated documentation files (the "Software"), to deal in the Software without restriction,
including without limitation the rights to use, copy, modify, merge, publish, distribute,
sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or
substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT
NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT
OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
