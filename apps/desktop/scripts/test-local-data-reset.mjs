import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const [reset, commands, lib, trash, storage, bridge, domain, app, ui, preview] = await Promise.all([
  readFile(new URL("../src-tauri/src/local_data_reset.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/commands.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/platform_trash.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/storage.rs", import.meta.url), "utf8"),
  readFile(new URL("../src/bridge.ts", import.meta.url), "utf8"),
  readFile(new URL("../src/domain.ts", import.meta.url), "utf8"),
  readFile(new URL("../src/App.tsx", import.meta.url), "utf8"),
  readFile(new URL("../src/ui.tsx", import.meta.url), "utf8"),
  readFile(new URL("../src/preview.ts", import.meta.url), "utf8"),
]);

assert.match(
  lib,
  /let context = tauri::generate_context!\(\);[\s\S]*let identifier = context\.config\(\)\.identifier\.clone\(\);[\s\S]*local_data_reset::apply_pending\(&identifier\)[\s\S]*tauri::Builder::default/u,
  "a pending reset must finish before Tauri opens the database or WebView",
);
assert.match(reset, /enum ResetIntentState \{[\s\S]*Collecting,[\s\S]*Staged,/u);
for (const target of ["AppData", "Cache", "WebKit", "Preferences"]) {
  assert.match(reset, new RegExp(`ResetTargetKind::${target}`, "u"));
}
assert.doesNotMatch(
  reset.match(/const ALL:[\s\S]*?\];/u)?.[0] || "",
  /Workspace|Markdown|AttachmentDirectory/u,
  "user workspaces must never enter the app-owned reset target list",
);
assert.match(reset, /stage_targets[\s\S]*write_json_atomic\(&layout\.intent_path, &intent, true\)[\s\S]*move_to_trash/u);
assert.match(reset, /trash_failure_rolls_every_target_back/u);
assert.match(reset, /partial_staging_is_resumed_without_overwriting/u);
assert.match(reset, /a_late_profile_blocker_cancels_before_staging/u);
assert.match(reset, /wakegpt-local-data-reset-recovery-package/u);
assert.match(reset, /a_tampered_recovery_manifest_is_rolled_back_before_trash/u);
assert.match(reset, /symlinked_target_fails_closed_and_preserves_other_roots/u);
assert.match(reset, /acknowledge_notice[\s\S]*occurred_at_ms/u);
assert.match(trash, /move_directory_to_system_trash/u);

assert.match(storage, /pub\(crate\) fn local_data_reset_inventory/u);
assert.match(commands, /LOCAL_DATA_RESET_CONFIRMATION: &str = "清除 WakeGPT"/u);
assert.match(commands, /MessageDialogKind::Warning/u);
assert.match(commands, /MessageDialogButtons::OkCancelCustom/u);
assert.match(commands, /blocking_show\(\)/u);
assert.ok(
  commands.indexOf(".blocking_show()") < commands.indexOf("app.autolaunch().disable()"),
  "native execution confirmation must happen before any login-item or reset side effect",
);
assert.match(commands, /pending_recovery_operations > 0/u);
assert.match(commands, /DefaultIdentityRuntimeState::Running[\s\S]*defaultIdentityRunning/u);
assert.match(commands, /app\.autolaunch\(\)\.disable/u);
assert.match(commands, /control\.set_paused\(true\)/u);
assert.match(commands, /local_data_reset::schedule\(&app\.config\(\)\.identifier\)/u);
assert.match(commands, /restart_app\.request_restart\(\)/u);
assert.match(commands, /preserves_workspace_files: true/u);
assert.match(lib, /collecting_app_data_dir[\s\S]*default_identity_runtime_probe[\s\S]*cancel_collecting/u);

assert.match(domain, /export interface LocalDataResetPreview/u);
assert.match(bridge, /localDataResetPreview:[\s\S]*local_data_reset_preview/u);
assert.match(bridge, /acknowledgeLocalDataResetNotice/u);
assert.match(app, /wakeBridge\.resetLocalData\(\{[\s\S]*confirmationPhrase/u);
assert.match(app, /wakeBridge\.acknowledgeLocalDataResetNotice\(notice\.occurredAtMs\)/u);
assert.match(ui, /export function LocalDataResetDialog/u);
assert.match(ui, /输入“\{preview\.confirmationPhrase\}”以确认/u);
assert.match(ui, /工作区 Markdown、工作区附件、外部导出包和系统备份/u);
assert.match(ui, /disabled=\{busy \|\| !matches\}/u);
assert.match(preview, /case "local_data_reset_preview"/u);
assert.match(preview, /case "reset_local_data"/u);

console.log("local data reset: ok");
