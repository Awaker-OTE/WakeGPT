import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const read = (relativePath) => readFile(new URL(relativePath, import.meta.url), "utf8");
const [storage, commands, bridge, app, ui, preview] = await Promise.all([
  read("../src-tauri/src/storage.rs"),
  read("../src-tauri/src/commands.rs"),
  read("../src/bridge.ts"),
  read("../src/App.tsx"),
  read("../src/ui.tsx"),
  read("../src/preview.ts"),
]);

assert.match(storage, /pub const SCHEMA_VERSION: u32 = 21;/u);
assert.match(storage, /pub fn disconnect_workspace\(/u);
assert.match(storage, /UPDATE workspaces[\s\S]{0,80}SET is_connected = 0/u);
assert.match(storage, /workspace_disconnection_is_reversible_and_preserves_local_state/u);
assert.match(storage, /Retained inbox record/u);
assert.match(storage, /Retained draft/u);
assert.match(commands, /pub fn disconnect_workspace\(/u);
assert.match(bridge, /invoke<UiPreferences>\("disconnect_workspace"/u);
assert.match(ui, /移除工作区/u);
assert.match(app, /保留它的收件箱记录、速记本、草稿、设置以及工作区中的全部 Markdown 和图片/u);
assert.match(preview, /case "disconnect_workspace"/u);
assert.doesNotMatch(
  commands.match(/pub fn disconnect_workspace\([\s\S]*?\n\}/u)?.[0] ?? "",
  /remove_(?:dir|file)|DELETE FROM|trash/u,
  "disconnecting a workspace must not delete application data or user files",
);

console.log("workspace disconnection contract: ok");
