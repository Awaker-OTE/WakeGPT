import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const [bridge, app, ui, commands, exportModule, storage, library] = await Promise.all([
  readFile(new URL("../src/bridge.ts", import.meta.url), "utf8"),
  readFile(new URL("../src/App.tsx", import.meta.url), "utf8"),
  readFile(new URL("../src/ui.tsx", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/commands.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/data_export.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/storage.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8"),
]);

assert.match(bridge, /exportLocalData:\s*\(\)\s*=>\s*invoke<DataExportResult \| null>\("export_local_data"\)/u);
assert.match(app, /const handleDataExport = async \(\) =>/u);
assert.match(app, /await wakeBridge\.exportLocalData\(\)/u);
assert.match(ui, /onExportLocalData: \(\) => void/u);
assert.match(ui, />导出本地数据</u);
assert.match(ui, /不包含 ChatGPT profile、Cookie 或令牌/u);
assert.match(commands, /#\[tauri::command\]\s*pub async fn export_local_data/u);
assert.match(commands, /\.blocking_pick_folder\(\)/u);
assert.match(storage, /VACUUM INTO \?1/u);
assert.match(exportModule, /wakegpt-export-v1\.json/u);
assert.match(exportModule, /read_record_preview/u);
assert.match(exportModule, /read_pending_preview/u);
assert.match(exportModule, /portable-local-data-export-not-restore-backup/u);
assert.match(
  exportModule,
  /excluded_data:\s*\[[\s\S]*?"local-diagnostics",\s*\],/u,
  "portable local-data exports must declare local diagnostics as excluded",
);
assert.match(
  exportModule,
  /assert!\(!package\.join\("diagnostics-v1"\)\.exists\(\)\)/u,
  "the export regression must prove the independent diagnostics directory is not copied",
);
assert.match(library, /commands::export_local_data/u);

console.log("local data export contract: ok");
