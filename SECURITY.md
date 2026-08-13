# Security Policy

## Supported versions

WakeGPT currently has no supported public release. Security fixes are developed against the latest reviewed source state; this is not a compatibility or response-time promise.

## Reporting a vulnerability

Once this repository is public, use the repository's **Security** tab and choose **Report a vulnerability** to open a private GitHub Security Advisory. Do not open a public Issue for a suspected vulnerability.

Include only the minimum information needed to reproduce the problem:

- affected source revision or application version;
- operating system and architecture;
- expected and observed behavior;
- a minimal reproduction using synthetic data;
- likely impact and any safe mitigation already tested.

Do not send passwords, tokens, cookies, private keys, account exports, real prompts, real attachments, private workspace files, production databases, or unredacted logs. WakeGPT maintainers will never ask for sign-in factors or a copy of another application's authenticated session.

If private vulnerability reporting is not available, do not publish exploit details. Wait until the repository owner provides a verified private channel in this file.

## Security boundaries

Reports are especially useful for workspace path escape, symlink races, destructive-operation bypass, credential exposure, loopback endpoint exposure, ChatGPT host-contract bypass, Markdown rendering, update verification, export/reset recovery, and secret or personal-data leakage.

No bug bounty, disclosure deadline, or guaranteed response time has been established. Publishing a fix or advisory requires an independently reviewed reproduction and regression check.
