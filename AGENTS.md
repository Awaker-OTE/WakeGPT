# WakeGPT Repository Rules

These rules apply to every file in this repository. They are intended to be safe for a public repository and may be made stricter by rules in a nearer directory.

## Product Boundary

- WakeGPT is independently designed and implemented. External products may inform user needs and observable behavior, but their source, styles, assets, configuration, schemas, private protocols, tests, data, and Git history must not be copied or mechanically rewritten.
- Restate referenced behavior as a neutral WakeGPT requirement before designing or implementing it.
- Do not imply an official partnership, certification, or endorsement by a model or tool provider.
- Use `构想`, `已定义`, `原型`, `已验证`, and `已交付` consistently. Planned or unverified behavior must not be described as supported, secure, or compatible.

## Cockpit Replacement Boundary

- WakeGPT is a replacement for Cockpit, not an extension, plugin, companion, or dependent service for Cockpit.
- WakeGPT source, runtime, development, tests, builds, packaging, instance discovery, configuration, persistence, migration, and verification must not depend on the Cockpit app, processes, services, registries, databases, configuration, private paths, APIs, source code, or build artifacts.
- WakeGPT must create and manage its own local state and required execution surfaces through independently implemented, documented platform or provider interfaces.
- A Codex process, debugging endpoint, configuration, or other environment prepared by Cockpit is not valid proof that a WakeGPT feature works. Reproduce the required setup through WakeGPT before marking the feature verified.
- Cockpit may be mentioned only as historical context or neutral evidence of a user need. If a WakeGPT path works only while Cockpit is installed or running, treat that path as blocked rather than supported.
- Current explicit user instructions may change WakeGPT's replacement scope. A request to reproduce an observable outcome does not authorize a Cockpit runtime or implementation dependency.

## Public Repository Boundary

- Treat every Git-tracked file and every committed history entry as public disclosure.
- Never commit personal names, home-directory paths, private hostnames, internal URLs, signing identities, backup locations, account inventories, or machine-specific initialization records.
- Never commit credentials, API keys, access tokens, refresh tokens, cookies, sessions, private keys, certificates containing private material, account exports, production databases, or unredacted operational logs.
- Test fixtures, screenshots, recordings, crash reports, and examples must use synthetic or irreversibly sanitized data.
- Personal working preferences belong in the user's global agent configuration, not this repository. A local root `AGENTS.override.md`, when used, must remain untracked and must preserve every privacy, security, provenance, and release rule in this file because it replaces the root `AGENTS.md` for Codex discovery.

## Privacy Hard Requirements

- Collect and retain the minimum data required for an explicitly defined user-facing purpose.
- Before adding a new data category or external transfer, document its purpose, source, destination, access boundary, retention period, deletion path, and user control.
- Telemetry and crash upload remain off by default until there is a public privacy notice, explicit user choice, redaction, retention limits, and a tested disable path.
- Users must have documented export and deletion paths for locally stored account metadata, prompts, outputs, attachments, remote-operation history, and audit records.
- Logs must redact secrets and minimize prompts, outputs, account identifiers, file contents, network addresses, and remote command results.
- Security or diagnostic collection must not silently become product analytics.

## Credentials And Accounts

- Store secrets only through an approved platform secure-storage mechanism. Do not add a plaintext fallback.
- Keep credentials isolated from ordinary application data, exports, backups, logs, and model context.
- Design rotation, revocation, expiry, deletion, and recovery before a credential format is considered stable.
- Manage only accounts and service credentials the user is authorized to use.
- Do not use account selection, pooling, or failover to evade access controls, provider limits, suspensions, pricing, or terms.
- Consumer login sharing, browser-cookie import, private endpoints, and UI automation are not acceptable production integration paths unless the provider explicitly authorizes that exact use.

## Live Account Test Gate

- Synthetic, local, and contract checks must run before any live-account test. A real provider account may be used only after the user explicitly authorizes that provider, account, purpose, and bounded test for the current task.
- ChatGPT/Codex official login must use the provider's official web authorization flow. WakeGPT may open that flow and observe its completion, but the user enters passwords, passkeys, one-time codes, and other sign-in factors directly on the provider page; WakeGPT must not request, type, read, store, or relay them.
- Do not extract or reuse credentials, cookies, sessions, account databases, or private runtime state from Cockpit, a browser, or another application for testing. Reauthorize through the official flow or let the user enter an authorized API key in WakeGPT's local interface.
- Live checks must be minimal, rate-bounded, non-destructive, and stopped after the required evidence or the first authentication, policy, quota, or risk error. Do not repeatedly retry login, evade limits, bypass provider controls, or use any method that could reasonably increase suspension or account-loss risk.
- Cockpit may be inspected read-only for observable workflow evidence. Its stored accounts are not test authorization, and a Cockpit-assisted result is not proof that WakeGPT works independently.

## Models And Remote Operations

- Treat model output, web content, files, tool results, and remote responses as untrusted input.
- Never turn model text directly into shell commands, privilege grants, credential access, network exposure, or destructive actions. Validate the concrete action through an independent policy boundary.
- Remote operations are default-deny and require an identified target, explicit scope, user authorization, bounded duration, cancellation, timeout, and an auditable result.
- Display the real operation and target before confirmation. Destructive, privileged, high-cost, or externally visible actions require a separate execution-time confirmation.
- Services bind to a local-only interface by default. External access requires explicit enablement, authentication, encryption, rate limits, revocation, and documented exposure.
- Secrets must not be sent to a model or remote target unless that transfer is necessary, visible to the user, and covered by the documented data flow.

## Providers, Dependencies, And Assets

- Use official or explicitly authorized provider interfaces and verify current documentation and terms before implementation or release.
- Record the source, version, license, purpose, and replacement path for every dependency, font, icon set, generated asset, copied snippet, and protocol definition.
- Do not introduce material with unknown provenance or an incompatible license.
- Provider names and trademarks may describe verified interoperability only; they must not become WakeGPT branding or imply official status.

## Engineering Habits

- Prefer the smallest durable change. Use platform features, the standard library, and already-approved dependencies before adding new machinery.
- Base repository claims on code and Git evidence, runtime claims on runnable checks, and external API claims on current official documentation.
- Keep edits scoped, avoid unrelated refactors, and add abstractions only when they remove demonstrated complexity.
- Non-trivial behavior leaves a proportional runnable check. Security, privacy, migration, and remote-operation paths require failure-case coverage.
- Use repository-relative commands and paths in tracked documentation.
- Keep generated output separate from source. Do not commit build products merely to publish them.
- Do not commit, push, publish, or create a release without explicit authorization.

## Release Gate

A public source push or distributable release is blocked until every applicable item below is satisfied:

- The source license, copyright owner, contribution policy, supported platforms, and release audience are decided and documented.
- The candidate tree and full Git history pass secret, personal-path, provenance, and third-party-license review.
- Relevant tests, security checks, privacy checks, upgrade or migration checks, backup or restore checks, and rollback checks pass.
- User-visible claims match verified behavior and current provider compatibility.
- Privacy notices, security reporting instructions, release notes, data deletion guidance, and known limitations are current.
- Distributable artifacts are built from a clean version tag, signed where the target platform requires it, checksummed, and accompanied by the applicable SBOM and third-party notices.
- Release artifacts are uploaded through the release system. Generated `dist/` or release-output directories remain untracked.

Any unresolved credential leak, privacy regression, unauthorized provider path, destructive-operation bypass, provenance gap, or failed rollback is a release blocker, regardless of schedule.
