import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const [domain, storage, fileSync, commands, library, bridge, app, ui, preview, imageInput] =
  await Promise.all([
    readFile(new URL("../src-tauri/src/domain.rs", import.meta.url), "utf8"),
    readFile(new URL("../src-tauri/src/storage.rs", import.meta.url), "utf8"),
    readFile(new URL("../src-tauri/src/file_sync.rs", import.meta.url), "utf8"),
    readFile(new URL("../src-tauri/src/commands.rs", import.meta.url), "utf8"),
    readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8"),
    readFile(new URL("../src/bridge.ts", import.meta.url), "utf8"),
    readFile(new URL("../src/App.tsx", import.meta.url), "utf8"),
    readFile(new URL("../src/ui.tsx", import.meta.url), "utf8"),
    readFile(new URL("../src/preview.ts", import.meta.url), "utf8"),
    readFile(new URL("../src/image-input.ts", import.meta.url), "utf8"),
  ]);

assert.match(storage, /pub const SCHEMA_VERSION: u32 = 21;/u);
assert.match(storage, /CREATE_PRODUCT_SETTINGS_FRESH/u);
assert.match(storage, /VALUES \(1, 30,/u, "fresh installs must default to 30 days");
assert.match(storage, /CREATE_PRODUCT_SETTINGS_MIGRATION/u);
assert.match(storage, /VALUES \(1, NULL,/u, "existing installs must migrate to permanent retention");
assert.match(storage, /fn schema_twenty_preserves_existing_data_and_defaults_migrations_to_permanent_retention/u);
assert.match(storage, /fn trash_cleanup_requires_a_fresh_preview_and_excludes_unsafe_records/u);
assert.match(storage, /record\.applied_revision = record\.revision/u);
assert.match(storage, /receipt\.state = 'uncertain'/u);
assert.match(storage, /record trash cleanup preview is stale/u);
assert.match(storage, /notebook\.numbering_sync_pending = 1/u);
assert.match(storage, /notebook\.attachment_directory_sync_pending = 1/u);
assert.match(storage, /FROM notebook_conflicts conflict/u);
assert.match(storage, /wakegpt-record-trash-cleanup-v2/u);
assert.match(storage, /SELECT record_trash_retention_days\s+FROM product_settings/u);

assert.match(domain, /validate_notebook_scan_ignore_directories/u);
assert.match(domain, /notebook scan ignore directories must be exact workspace-relative directories/u);
assert.match(fileSync, /store\.notebook_scan_ignore_directories\(\)/u);
assert.match(fileSync, /relative == ignored \|\| relative\.starts_with\(ignored\)/u);
assert.doesNotMatch(
  fileSync,
  /Some\(\s*"\.git"[\s\S]*?"node_modules"/u,
  "recovery scanning must not keep a second hard-coded ignore list",
);

for (const command of [
  "product_settings",
  "set_product_settings",
  "preview_record_trash_cleanup",
  "purge_record_trash",
]) {
  assert.match(commands, new RegExp(`#\\[tauri::command\\]\\s*pub fn ${command}\\b`, "u"));
  assert.match(library, new RegExp(`commands::${command}\\b`, "u"));
  assert.match(bridge, new RegExp(`"${command}"`, "u"));
  assert.match(preview, new RegExp(`case "${command}"`, "u"));
}

assert.match(app, /wakeBridge\.productSettings\(\)/u);
assert.match(app, /wakeBridge\.previewRecordTrashCleanup\(\)/u);
assert.match(app, /wakeBridge\.purgeRecordTrash\(preview\.previewToken\)/u);
assert.match(ui, /ariaLabel="记录废纸篓保留期"/u);
assert.match(ui, /aria-label="恢复扫描内置忽略目录"/u);
assert.match(ui, /aria-label="额外恢复扫描忽略目录"/u);
assert.match(ui, /protectedNotebookScanIgnoreDirectories/u);
assert.match(ui, /PNG、JPEG、WebP、GIF/u);
assert.match(ui, /最多 20 MiB/u);
assert.match(ui, /最多 10 张，合计最多 100 MiB/u);

assert.match(imageInput, /MAX_IMAGES_PER_RECORD = 10/u);
assert.match(imageInput, /MAX_IMAGE_BYTES = 20 \* 1024 \* 1024/u);
assert.match(imageInput, /MAX_RECORD_IMAGE_BYTES = 100 \* 1024 \* 1024/u);
assert.match(storage, /set_codex_integration_paused/u);
assert.match(library, /product_settings\.codex_integration_paused/u);
assert.match(commands, /store\s*\.set_codex_integration_paused\(request\.paused\)/u);

console.log("product settings and record trash lifecycle: ok");
