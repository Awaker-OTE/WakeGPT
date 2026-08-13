import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const read = (relativePath) => readFile(new URL(relativePath, import.meta.url), "utf8");
const [app, bridge, domain, ui, card, commands, fileSync, storage, lib] = await Promise.all([
  read("../src/App.tsx"),
  read("../src/bridge.ts"),
  read("../src/domain.ts"),
  read("../src/ui.tsx"),
  read("../src-tauri/assets/codex_card_v1.js"),
  read("../src-tauri/src/commands.rs"),
  read("../src-tauri/src/file_sync.rs"),
  read("../src-tauri/src/storage.rs"),
  read("../src-tauri/src/lib.rs"),
]);

assert.match(storage, /pub const SCHEMA_VERSION: u32 = 21;/u);
assert.match(storage, /CREATE TABLE IF NOT EXISTS notebook_conflicts/u);
assert.match(storage, /file_version_adoption_pending/u);
assert.match(storage, /DELETE FROM notebook_conflicts/u);
assert.doesNotMatch(storage.match(/CREATE_NOTEBOOK_CONFLICTS:[\s\S]*?STRICT;";/u)?.[0] ?? "", /body_markdown/u);

assert.match(fileSync, /pub fn inspect_notebook_conflict/u);
assert.match(fileSync, /pub fn resolve_notebook_conflict/u);
assert.match(fileSync, /replace_existing_cas/u);
assert.match(fileSync, /conflict_evidence_stale/u);
assert.match(fileSync, /file_adoption_intent_recovers_after_the_file_was_installed/u);

assert.match(commands, /pub fn inspect_notebook_conflict/u);
assert.match(commands, /pub fn resolve_notebook_conflict/u);
assert.match(lib, /commands::inspect_notebook_conflict/u);
assert.match(lib, /commands::resolve_notebook_conflict/u);
assert.match(bridge, /inspectNotebookConflict/u);
assert.match(bridge, /resolveNotebookConflict/u);
assert.match(domain, /NotebookConflictInspection/u);

assert.match(ui, /export function ConflictInspectorDialog/u);
assert.match(ui, /采用 WakeGPT 版本/u);
assert.match(ui, /采用文件版本/u);
assert.match(ui, /解除管理/u);
assert.match(ui, /<pre>\{inspection\.fileMarkdown\}<\/pre>/u);
assert.doesNotMatch(ui, /dangerouslySetInnerHTML/u);
assert.match(app, /handleResolveNotebookConflict/u);
assert.match(app, /Promise\.allSettled\(\[refreshRecords\(\), refreshNotebooks\(\)\]\)/u);

assert.match(card, /const notebookBlocksChanges = \(notebook\)/u);
assert.match(card, /!notebookStateChanged[\s\S]*canRestoreInlineEdit\(inlineEditSnapshot, state\)/u);
assert.match(card, /&& !notebookBlocksChanges\(/u);

console.log("notebook conflict contract: ok");
