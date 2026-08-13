# Contributing to WakeGPT

WakeGPT is licensed under the [Apache License 2.0](LICENSE). Project-wide notices use
`Copyright 2026 WakeGPT Contributors`; individual contributors retain any copyright they own in
their contributions unless a separate written agreement states otherwise.

## Current contribution policy

- Discussion and synthetic bug reports may be opened after the repository becomes public.
- By intentionally submitting a contribution for inclusion in WakeGPT, you confirm that you have
  the right to submit it and license it under Apache-2.0. Apache-2.0 section 5 applies without
  additional terms unless a separate written agreement says otherwise.
- Clearly mark material that is shared only for discussion and is **Not a Contribution**.
- Never include credentials, account exports, authenticated sessions, real user content, personal paths, private hosts, signing material, or unredacted operational logs.
- Do not copy or mechanically rewrite source, styles, assets, schemas, private protocols, tests, configuration, or Git history from Cockpit, OpenAI products, or another project.
- Identify third-party code or assets before submission and include their exact source and license;
  material with unclear provenance or incompatible terms cannot be accepted.

## Development checks

From `apps/desktop/`:

```bash
npm ci
npm test
npm run build
cargo test --manifest-path src-tauri/Cargo.toml --locked -- --test-threads=1
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml --locked --all-targets --all-features -- -D warnings
```

Non-trivial behavior must include a proportional runnable regression check. Security, privacy, migration, recovery, file mutation and remote-operation changes must cover failure paths, not only success paths.

## Reports and proposals

Use synthetic examples. State the observed behavior, expected behavior, smallest reproduction, affected revision and checks already run. Feature proposals should identify the user need and safety boundary without assuming an unofficial provider interface is stable or authorized.

Security findings belong in the private process described by [SECURITY.md](SECURITY.md), never in a public Issue.
