import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const [domain, storage, commands, library, bridge, app, ui, preview] = await Promise.all([
  readFile(new URL("../src-tauri/src/domain.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/storage.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/commands.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8"),
  readFile(new URL("../src/bridge.ts", import.meta.url), "utf8"),
  readFile(new URL("../src/App.tsx", import.meta.url), "utf8"),
  readFile(new URL("../src/ui.tsx", import.meta.url), "utf8"),
  readFile(new URL("../src/preview.ts", import.meta.url), "utf8"),
]);

assert.match(domain, /pub struct WorkspaceOpenPreference/u);
assert.match(storage, /pub const SCHEMA_VERSION: u32 = 21;/u);
assert.match(storage, /CREATE_WORKSPACE_OPEN_PREFERENCES/u);
assert.match(storage, /fn schema_twenty_one_adds_workspace_open_preferences_without_changing_selection/u);
assert.match(storage, /fn notebook_display_name_and_workspace_open_preference_are_independent/u);
assert.match(storage, /pub fn rename_notebook\(/u);
assert.match(storage, /display_name = \?3 COLLATE NOCASE/u);
assert.match(storage, /workspace_open_notebook_id\(&connection, workspace_id\)/u);
assert.match(storage, /UPDATE workspace_open_preferences[\s\S]*default_notebook_id = NULL/u);

for (const command of [
  "rename_notebook",
  "workspace_open_preference",
  "set_workspace_open_preference",
]) {
  assert.match(commands, new RegExp(`#\\[tauri::command\\]\\s*pub fn ${command}\\b`, "u"));
  assert.match(library, new RegExp(`commands::${command}\\b`, "u"));
  assert.match(bridge, new RegExp(`"${command}"`, "u"));
  assert.match(preview, new RegExp(`case "${command}"`, "u"));
}

assert.match(commands, /emit_card_data_changed\([\s\S]*"notebooks"/u);
assert.match(app, /wakeBridge\.renameNotebook\(/u);
assert.match(app, /wakeBridge\.setWorkspaceOpenPreference\(/u);
assert.match(ui, /ariaLabel="工作区默认打开位置"/u);
assert.match(ui, /aria-label="当前速记本显示名称"/u);
assert.match(ui, /不重命名 Markdown 文件/u);
assert.match(ui, /上次使用的位置/u);

console.log("notebook display name and workspace open preference: ok");
