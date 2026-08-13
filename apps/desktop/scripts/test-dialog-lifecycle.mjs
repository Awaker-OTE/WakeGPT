import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const commandsUrl = new URL("../src-tauri/src/commands.rs", import.meta.url);
const [source, appCss, ui] = await Promise.all([
  readFile(commandsUrl, "utf8"),
  readFile(new URL("../src/App.css", import.meta.url), "utf8"),
  readFile(new URL("../src/ui.tsx", import.meta.url), "utf8"),
]);
const commandMarker = /#\[tauri::command\]/gu;
const starts = [...source.matchAll(commandMarker)].map((match) => match.index);
const blockingDialogCommands = [];

for (const [index, start] of starts.entries()) {
  const end = starts[index + 1] ?? source.length;
  const command = source.slice(start, end);
  if (!/\.blocking_(?:pick|save)_[a-z_]+\s*\(/u.test(command)) continue;

  const signature = /pub\s+(async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)\b/u.exec(command);
  assert.ok(signature, "every Tauri command with a blocking dialog must have a public function signature");
  blockingDialogCommands.push(signature[2]);
  assert.equal(
    signature[1],
    "async ",
    `${signature[2]} uses a blocking native dialog and must run as an async Tauri command`,
  );
}

assert.deepEqual(
  blockingDialogCommands.sort(),
  [
    "authorize_workspace",
    "bind_existing_notebook",
    "export_local_data",
    "export_local_diagnostics",
    "pick_record_images",
  ],
  "the native dialog command audit must cover every current Tauri entry point",
);
assert.match(appCss, /\.dialog-backdrop \{[^}]*overflow:\s*auto;/u);
assert.match(appCss, /\.dialog \{[^}]*max-height:\s*calc\(100vh - 48px\);[^}]*overflow:\s*auto;[^}]*scrollbar-gutter:\s*stable;/u);
assert.match(ui, /event\.stopPropagation\(\);[\s\S]*window\.addEventListener\("keydown", handleKeyDown, true\);/u);
assert.match(ui, /window\.removeEventListener\("keydown", handleKeyDown, true\);/u);
assert.match(ui, /const closeDisabledRef = useRef\(closeDisabled\);[\s\S]*closeDisabledRef\.current = closeDisabled;/u);

console.log("dialog lifecycle: ok");
