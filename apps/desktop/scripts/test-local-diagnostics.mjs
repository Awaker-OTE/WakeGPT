import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const [
  diagnostics,
  commands,
  library,
  dataExport,
  domain,
  bridge,
  app,
  ui,
  preview,
] = await Promise.all([
  readFile(new URL("../src-tauri/src/diagnostics.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/commands.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/data_export.rs", import.meta.url), "utf8"),
  readFile(new URL("../src/domain.ts", import.meta.url), "utf8"),
  readFile(new URL("../src/bridge.ts", import.meta.url), "utf8"),
  readFile(new URL("../src/App.tsx", import.meta.url), "utf8"),
  readFile(new URL("../src/ui.tsx", import.meta.url), "utf8"),
  readFile(new URL("../src/preview.ts", import.meta.url), "utf8"),
]);

const compact = (source) => source.replace(/\s+/gu, " ");
const compactDiagnostics = compact(diagnostics);

assert.match(diagnostics, /const DIAGNOSTICS_DIRECTORY: &str = "diagnostics-v1";/u);
assert.match(diagnostics, /const DATABASE_FILE_NAME: &str = "wakegpt-diagnostics\.sqlite3";/u);
assert.match(diagnostics, /const DIAGNOSTICS_SCHEMA_VERSION: u32 = 1;/u);
assert.match(library, /LocalDiagnostics::new\(&app_data_dir\)/u);
assert.match(library, /app_data_dir\.join\("wakegpt\.sqlite3"\)/u);
assert.match(diagnostics, /pragma_update\(None, "journal_mode", "DELETE"\)/u);
assert.match(diagnostics, /pragma_update\(None, "secure_delete", "ON"\)/u);
assert.match(diagnostics, /pragma_update\(None, "auto_vacuum", "FULL"\)/u);
assert.match(diagnostics, /builder\.mode\(0o700\)/u);
assert.match(diagnostics, /fs::Permissions::from_mode\(0o600\)/u);

assert.ok(
  compactDiagnostics.includes("const ALLOWED_RETENTION_DAYS: [u32; 4] = [1, 3, 7, 14];"),
  "diagnostics retention must remain a fixed allowlist",
);
assert.ok(
  compactDiagnostics.includes(
    "const ALLOWED_MAX_BYTES: [u64; 4] = [ 1024 * 1024, 5 * 1024 * 1024, 10 * 1024 * 1024, DEFAULT_MAX_BYTES, ];",
  ),
  "diagnostics storage limits must remain a fixed allowlist",
);
assert.match(
  compactDiagnostics,
  /matches!\(severity, "info" \| "warning" \| "error"\)/u,
  "diagnostic severity must fail closed to its fixed values",
);
assert.match(
  compactDiagnostics,
  /"app" \| "recovery" \| "codex" \| "card" \| "default_identity" \| "lifecycle"/u,
  "diagnostic subsystems must fail closed to their fixed values",
);
assert.match(diagnostics, /fn valid_code\(code: &str\)[\s\S]*?is_ascii_lowercase\(\)[\s\S]*?is_ascii_digit\(\)/u);
assert.match(diagnostics, /const MAX_CONTEXT_BYTES: usize = 2048;/u);
assert.match(diagnostics, /fn untrusted_canaries_are_rejected_and_never_enter_the_database/u);
assert.match(diagnostics, /fn clear_scrubs_deleted_event_bytes_and_preserves_settings/u);

for (const excluded of [
  "record-bodies",
  "markdown-file-contents",
  "image-bytes",
  "absolute-paths",
  "account-identities",
  "cookies",
  "authentication-tokens",
  "network-addresses",
]) {
  assert.match(
    diagnostics,
    new RegExp(`"${excluded}"`, "u"),
    `diagnostics exports must declare ${excluded} as excluded`,
  );
}
assert.match(
  dataExport,
  /excluded_data:\s*\[[\s\S]*?"local-diagnostics",\s*\],/u,
  "portable local-data exports must explicitly exclude local diagnostics",
);
assert.match(
  diagnostics,
  /const EVENTS_FILE_NAME: &str = "wakegpt-diagnostics-v1\.jsonl";/u,
);
assert.match(
  diagnostics,
  /const MANIFEST_FILE_NAME: &str = "wakegpt-diagnostics-manifest-v1\.json";/u,
);

const commandContracts = [
  ["local_diagnostics_status", "localDiagnosticsStatus"],
  ["list_local_diagnostics", "listLocalDiagnostics"],
  ["update_local_diagnostics_settings", "updateLocalDiagnosticsSettings"],
  ["export_local_diagnostics", "exportLocalDiagnostics"],
  ["clear_local_diagnostics", "clearLocalDiagnostics"],
];
for (const [command, bridgeMethod] of commandContracts) {
  assert.match(
    commands,
    new RegExp(`#\\[tauri::command\\]\\s*pub (?:async )?fn ${command}\\b`, "u"),
    `${command} must remain a Tauri command`,
  );
  assert.match(
    library,
    new RegExp(`commands::${command}\\b`, "u"),
    `${command} must remain registered`,
  );
  assert.match(
    bridge,
    new RegExp(`${bridgeMethod}:[\\s\\S]{0,180}?"${command}"`, "u"),
    `${bridgeMethod} must call ${command}`,
  );
  assert.match(
    preview,
    new RegExp(`case "${command}"`, "u"),
    `${command} must remain available in UI preview mode`,
  );
}

for (const typeName of [
  "LocalDiagnosticsStatus",
  "LocalDiagnosticEvent",
  "LocalDiagnosticsPage",
  "LocalDiagnosticsExportResult",
]) {
  assert.match(domain, new RegExp(`export interface ${typeName}\\b`, "u"));
}

assert.match(app, /wakeBridge\.localDiagnosticsStatus\(\)/u);
assert.match(app, /wakeBridge\.listLocalDiagnostics\(\{ beforeId, limit: 50 \}\)/u);
assert.match(app, /wakeBridge\.updateLocalDiagnosticsSettings\(nextSettings\)/u);
assert.match(app, /wakeBridge\.exportLocalDiagnostics\(\)/u);
assert.match(app, /wakeBridge\.clearLocalDiagnostics\(\)/u);
assert.match(ui, /ariaLabel="本地诊断保留时间"/u);
assert.match(ui, /ariaLabel="本地诊断存储上限"/u);
assert.match(ui, /onClick=\{onOpenLocalDiagnostics\}[\s\S]{0,160}查看诊断/u);
assert.match(ui, /正在导出…" : "单独导出"/u);
assert.match(ui, /正在清空…" : "清空"/u);

assert.match(
  app,
  /<ConfirmDialog\s+open=\{localDiagnosticsClearOpen\}\s+title="清空本地诊断？"\s+confirmLabel="清空诊断"\s+busy=\{busyAction === "diagnostics-clear"\}\s+destructive/u,
  "clearing diagnostics must require its own destructive confirmation",
);
assert.match(app, /将永久删除 WakeGPT 当前诊断库中的全部事件/u);

assert.match(ui, /workspace: Workspace \| null;/u);
assert.match(ui, /onClick=\{\(\) => onViewChange\("settings"\)\}/u);
assert.match(ui, /\{workspace \? <section className="settings-group">/u);
assert.match(
  app,
  /\{view === "settings" \? \(\s*<SettingsView\s+workspace=\{activeWorkspace\}/u,
  "global settings must render before the active-workspace branch",
);
assert.match(
  app,
  /\) : activeWorkspace \? \(/u,
  "the empty-workspace screen must not replace global settings",
);

console.log("local diagnostics contract: ok");
