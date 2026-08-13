import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const [cargo, rust, install, storage, sync, attachments, commands, adapter, lib, domain, bridge, app, ui, config, capability] = await Promise.all([
  readFile(new URL("../src-tauri/Cargo.toml", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/updates.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/update_install.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/storage.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/file_sync.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/attachments.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/commands.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/codex_adapter.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8"),
  readFile(new URL("../src/domain.ts", import.meta.url), "utf8"),
  readFile(new URL("../src/bridge.ts", import.meta.url), "utf8"),
  readFile(new URL("../src/App.tsx", import.meta.url), "utf8"),
  readFile(new URL("../src/ui.tsx", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/capabilities/default.json", import.meta.url), "utf8"),
]);

assert.match(cargo, /tauri-plugin-updater = "=2\.10\.1"/u);
assert.match(cargo, /reqwest = \{ version = "=0\.13\.4"/u);
assert.match(cargo, /minisign-verify = "=0\.2\.5"/u);
assert.match(rust, /option_env!\("WAKEGPT_UPDATER_PUBLIC_KEY"\)/u);
assert.match(rust, /option_env!\("WAKEGPT_UPDATER_ENDPOINT"\)/u);
assert.match(rust, /const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs\(30\)/u);
assert.match(rust, /pub const UPDATER_TARGET: &str = "darwin-universal"/u);
assert.match(rust, /strip_prefix\("https:\/\/github\.com\/"\)/u);
assert.match(rust, /path\.contains\("\/releases\/latest\/download\/"\)/u);
assert.match(rust, /verify_stream\(&signature\)/u);
assert.match(rust, /UPDATE_DOWNLOAD_TIMEOUT/u);
assert.match(rust, /release-assets\.githubusercontent\.com/u);
assert.match(rust, /UpdatePhase::Verifying/u);
assert.match(rust, /UpdatePhase::ReadyToInstall/u);
assert.match(rust, /freeze_for_update/u);
assert.match(rust, /external_network_enabled/u);
assert.match(rust, /automatic_downloads_enabled/u);
assert.doesNotMatch(rust, /download_and_install/u);
assert.match(install, /pub\(crate\) const MAX_UPDATE_BYTES: u64 = 256 \* 1024 \* 1024/u);
assert.match(install, /HEALTH_PROBE_ARGUMENT/u);
assert.match(install, /run_health_probe_if_requested/u);
assert.match(install, /store\.backup_database_for_update/u);
assert.match(install, /update\.install\(bytes\)/u);
assert.match(storage, /update_freeze_drains_and_blocks_database_operations/u);
assert.match(attachments, /update_freeze_blocks_attachment_staging_until_the_guard_is_released/u);
assert.match(sync, /execute_record_mutation_with_attachments/u);
assert.match(commands, /execute_record_mutation_with_attachments\(&store, &attachments/u);
assert.match(adapter, /execute_record_mutation_with_attachments\(&store, &coordinator/u);
assert.match(adapter, /pub\(crate\) fn wait_until_paused\(&self, timeout: Duration\)/u);
const installBody = install.slice(install.indexOf("pub(crate) fn install_prepared"));
assert.ok(
  installBody.indexOf("spawn_guardian(&intent") < installBody.indexOf("update.install(bytes)"),
  "guardian must start before the updater changes the application",
);
const installCommandBody = rust.slice(rust.indexOf("pub async fn install_downloaded_update"));
assert.ok(
  installCommandBody.indexOf("wait_until_paused(Duration::from_secs(10))") <
    installCommandBody.indexOf("freeze_for_update()"),
  "Codex card sessions must drain before update write locks and database backup",
);
assert.ok(
  installCommandBody.indexOf("runtime\n                    .finish_restarting()") <
    installCommandBody.indexOf("update_install::install_prepared("),
  "all fallible runtime state changes must finish before the application is replaced",
);
assert.ok(
  installCommandBody.indexOf("update_install::install_prepared(") <
    installCommandBody.indexOf("app_for_install.exit(0)"),
  "the parent must exit after the guardian takes ownership of an install",
);
assert.doesNotMatch(
  installCommandBody,
  /launch\.transaction_id/u,
  "no fallible validation may keep the parent alive after installation starts",
);
assert.match(
  installCommandBody,
  /Err\(_\) => \{[\s\S]*?recover_after_install_worker_failure[\s\S]*?Ok\(false\)[\s\S]*?Ok\(true\) \| Err\(_\)[\s\S]*?app\.exit\(1\);[\s\S]*?std::process::exit\(1\);/u,
  "a panicked worker may return only after proving that no install intent exists",
);

assert.match(domain, /export interface UpdateStatus/u);
for (const command of [
  "update_status",
  "set_update_settings",
  "check_for_updates",
  "skip_update_version",
  "download_update",
  "discard_downloaded_update",
  "install_downloaded_update",
  "acknowledge_update_result",
  "open_update_release",
]) {
  assert.match(bridge, new RegExp(`"${command}"`, "u"));
  assert.match(lib, new RegExp(`updates::${command}`, "u"));
}
assert.match(app, /wakeBridge\.checkForUpdates\(false\)/u);
assert.match(app, /wakeBridge\.checkForUpdates\(true\)/u);
assert.match(app, /next\.automaticDownloadsEnabled/u);
assert.match(app, /wakeBridge\.downloadUpdate\(next\.availableVersion\)/u);
assert.match(app, /60 \* 60 \* 1_000/u);
assert.match(app, /wakegpt:\/\/update-download-progress/u);
assert.match(app, /persistCurrentDraft\(\)/u);
assert.match(ui, /当前本地构建未嵌入正式公钥和 HTTPS 端点，因此不会联网/u);
assert.match(ui, /下载完成，正在验证签名并安全保存/u);
assert.match(ui, /重启并更新/u);
assert.match(ui, /允许 WakeGPT 访问外部更新网络/u);
assert.match(ui, /后台自动下载 WakeGPT 更新/u);
assert.match(ui, /<progress/u);
const updaterBootstrapConfig = JSON.parse(config).plugins?.updater;
assert.deepEqual(updaterBootstrapConfig, {
  pubkey: "",
  endpoints: [],
});
assert.doesNotMatch(config, /dangerousInsecureTransportProtocol\s*"?\s*:\s*true/u);
assert.doesNotMatch(capability, /updater:/u);

console.log("signed updater delivery contract: ok");
