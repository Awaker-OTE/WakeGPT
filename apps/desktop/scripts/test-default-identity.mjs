import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const read = (relativePath) => readFile(new URL(relativePath, import.meta.url), "utf8");
const [storage, commands, adapter, bridge, ui] = await Promise.all([
  read("../src-tauri/src/storage.rs"),
  read("../src-tauri/src/commands.rs"),
  read("../src-tauri/src/codex_adapter.rs"),
  read("../src/bridge.ts"),
  read("../src/ui.tsx"),
]);

assert.match(storage, /pub const SCHEMA_VERSION: u32 = 21;/u);
assert.match(storage, /const DEFAULT_IDENTITY_SCHEMA_VERSION: u32 = 12;/u);
const table = storage.match(
  /const CREATE_DEFAULT_IDENTITY_SLOT:[\s\S]*?\) STRICT;";/u,
)?.[0];
assert.ok(table, "schema v12 must own a default identity singleton table");
assert.doesNotMatch(
  table,
  /(?:path|email|cookie|token|credential|account_id)/iu,
  "the default identity table may store configuration, never profile paths or credentials",
);

const request = commands.match(
  /pub struct ConfigureDefaultIdentityRequest \{([\s\S]*?)\n\}/u,
)?.[1];
assert.ok(request, "the default identity command request must be explicit");
assert.match(request, /pub alias: String,/u);
assert.match(request, /pub locked: bool,/u);
assert.doesNotMatch(request, /path|email|cookie|token|credential/iu);

assert.match(adapter, /const DEFAULT_IDENTITY_PROFILE_DIRECTORY: &str/u);
assert.match(adapter, /chatgpt_open_command\(app_bundle, 0, Some\(profile\.path\(\)\)\)/u);
assert.match(adapter, /default_identity_profile_occupied/u);
assert.match(adapter, /Permissions::from_mode\(0o700\)/u);

for (const command of [
  "default_identity_status",
  "configure_default_identity",
  "use_default_identity",
  "unbind_default_identity",
]) {
  assert.ok(bridge.includes(`"${command}"`), `bridge must expose ${command}`);
}
assert.match(ui, /OpenAI 官方登录页/u);
assert.match(ui, /仅保存别名与锁定状态/u);
assert.match(ui, /不读取或保存账号邮箱/u);

console.log("default identity boundary: ok");
