import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const read = (path) => readFileSync(resolve(root, path), "utf8");

const cargo = read("src-tauri/Cargo.toml");
const lib = read("src-tauri/src/lib.rs");
const lifecycle = read("src-tauri/src/lifecycle.rs");
const commands = read("src-tauri/src/commands.rs");
const capability = read("src-tauri/capabilities/default.json");
const domain = read("src/domain.ts");
const bridge = read("src/bridge.ts");
const app = read("src/App.tsx");
const ui = read("src/ui.tsx");
const preview = read("src/preview.ts");
const notices = read(resolve(root, "../../NOTICE.md"));

assert.match(cargo, /tauri-plugin-autostart\s*=\s*"=2\.5\.1"/u);
assert.match(lib, /tauri_plugin_autostart::init\([\s\S]*MacosLauncher::LaunchAgent/u);
assert.match(lib, /lifecycle::AUTOSTART_ARGUMENT/u);
assert.match(
  lib,
  /lifecycle::apply_startup_visibility\(app, local_data_reset_notice_pending\)/u,
);

assert.match(
  lifecycle,
  /pub const AUTOSTART_ARGUMENT:\s*&str\s*=\s*"--wakegpt-autostart"/u,
);
assert.match(lifecycle, /pub fn is_autostart_launch/u);
assert.match(lifecycle, /local_data_reset_notice_pending \|\| !is_autostart_launch/u);
assert.match(lifecycle, /AUTOSTART_ARGUMENT[\s\S]*window\.hide\(\)/u);

assert.match(commands, /pub struct LoginItemStatus/u);
assert.match(commands, /pub struct SetLoginItemEnabledRequest/u);
assert.match(commands, /pub fn login_item_status/u);
assert.match(commands, /pub fn set_login_item_enabled/u);
assert.match(commands, /login_item_verification_failed/u);
assert.match(lib, /commands::login_item_status/u);
assert.match(lib, /commands::set_login_item_enabled/u);

assert.match(domain, /export interface LoginItemStatus/u);
assert.match(bridge, /loginItemStatus:\s*\(\)/u);
assert.match(bridge, /setLoginItemEnabled:\s*\(enabled:\s*boolean\)/u);
assert.match(app, /wakeBridge\.loginItemStatus\(\)/u);
assert.match(app, /wakeBridge\.setLoginItemEnabled\(nextEnabled\)/u);
assert.match(ui, /aria-label="登录时启动 WakeGPT"/u);
assert.match(ui, /role="switch"/u);
assert.match(ui, /登录后在后台启动；主窗口保持隐藏/u);
assert.match(preview, /case "login_item_status":/u);
assert.match(preview, /case "set_login_item_enabled":/u);

assert.doesNotMatch(
  capability,
  /autostart:/u,
  "the WebView must use WakeGPT's narrow commands, not direct autostart plugin permissions",
);
assert.match(notices, /Tauri Autostart 2\.5\.1/u);

console.log("login item contract: ok");
