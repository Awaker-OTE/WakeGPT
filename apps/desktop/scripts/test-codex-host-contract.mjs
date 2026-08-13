import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";

const [card, adapter, cargo, domain, ui] = await Promise.all([
  readFile(new URL("../src-tauri/assets/codex_card_v1.js", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/codex_adapter.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/Cargo.toml", import.meta.url), "utf8"),
  readFile(new URL("../src/domain.ts", import.meta.url), "utf8"),
  readFile(new URL("../src/ui.tsx", import.meta.url), "utf8"),
]);

assert.match(cargo, /objc2-foundation[^\n]*"NSBundle"/u);
assert.match(adapter, /SUPPORTED_CODEX_VERSION: &str = "26\.803\.81509"/u);
assert.match(adapter, /SUPPORTED_CODEX_BUILD: &str = "6415"/u);
assert.match(adapter, /HOST_CONTRACT_ID: &str = "chatgpt-26\.803\.81509-6415"/u);
assert.match(adapter, /codex_host_contract_error\([\s\S]*instance\.codex_version[\s\S]*instance\.codex_build/u);
assert.match(adapter, /verify_running_chatgpt_host_contract\(target\.process_id\)\?/u);
assert.match(adapter, /incompatible_process_ids[\s\S]*worker\.cancelled\.store\(true, Ordering::Release\)/u);
assert.match(adapter, /host\.dataset\.hostContractId===\{contract_json\}/u);
assert.match(card, /const hostContractId = "chatgpt-26\.803\.81509-6415"/u);
assert.match(card, /bootstrap\.hostContractId !== hostContractId/u);
assert.match(card, /contextCardResolution\(\)\.state === "ambiguous"/u);
assert.match(card, /send\("adapterIncompatible", \{ code: "contextSelectorAmbiguous" \}\)/u);
assert.match(adapter, /"contextSelectorAmbiguous" => Err\(AdapterRuntimeError::Protocol/u);
assert.match(adapter, /globalThis\.__wakegptComposerImageInput\?\.\(\) \|\| null/u);
assert.doesNotMatch(adapter, /inputs\.find\(\(input\).*input\.accept/u);
assert.match(adapter, /"Input\.dispatchDragEvent"[\s\S]*"dragEnter"[\s\S]*"dragOver"[\s\S]*"drop"/u);
assert.match(card, /globalThis\.__wakegptComposerAttachmentMode = \(\) =>/u);
assert.match(card, /globalThis\.__wakegptComposerDropPoint = \(\) =>/u);
assert.match(domain, /detectedCodexVersion: string \| null/u);
assert.match(domain, /\| "incompatible"/u);
assert.match(ui, /incompatible: "版本未验证，已停止注入"/u);

for (const functionName of ["restart_codex_with_integration", "restart_codex_instance_with_integration"]) {
  const start = adapter.indexOf(`pub fn ${functionName}`);
  assert.notEqual(start, -1, `${functionName} must exist`);
  const end = adapter.indexOf("\n#[cfg", start + 1);
  const body = adapter.slice(start, end === -1 ? undefined : end);
  assert.ok(
    body.indexOf("verify_chatgpt_host_contract(app_bundle)?") < body.indexOf(".terminate()"),
    `${functionName} must reject an unknown host before terminating ChatGPT`,
  );
}

const marker = "globalThis.__wakegptMountCodexCard = mount;";
assert.ok(card.includes(marker));
const instrumented = card.replace(
  marker,
  `globalThis.__wakegptHostContractTest = { resolveContextCandidates };\n  ${marker}`,
);

let composers = [];
class FakeElement {
  constructor() {
    this.parentElement = null;
    this.disabled = false;
    this.accept = "";
    this.fileInputs = [];
  }

  closest() { return null; }

  getBoundingClientRect() {
    return { left: 100, top: 600, right: 740, bottom: 700, width: 640, height: 100 };
  }

  querySelector(selector) {
    return selector === "input[type=file]" ? this.fileInputs[0] || null : null;
  }

  querySelectorAll(selector) {
    return selector === "input[type=file]" ? this.fileInputs : [];
  }
}

const context = vm.createContext({
  console,
  document: { querySelectorAll: () => composers, body: {} },
  globalThis: null,
  HTMLElement: FakeElement,
  HTMLTextAreaElement: class extends FakeElement {},
  InputEvent: class {},
  MutationObserver: class {},
  ResizeObserver: class {},
  window: { innerWidth: 1280, innerHeight: 720 },
  getComputedStyle: () => ({ display: "block", visibility: "visible" }),
});
context.globalThis = context;
vm.runInContext(instrumented, context, { filename: "codex_card_v1.js" });

const resolve = context.__wakegptHostContractTest.resolveContextCandidates;
const rect = { left: 900, top: 80, right: 1220, bottom: 400 };
assert.equal(resolve([]).state, "missing");
assert.equal(resolve([{ node: { id: "a" }, rect }]).state, "ready");
assert.equal(resolve([
  { node: { id: "outer" }, rect },
  { node: { id: "inner" }, rect: { left: 902, top: 82, right: 1218, bottom: 398 } },
]).state, "ready", "nested geometry must de-duplicate");
assert.equal(resolve([
  { node: { id: "a" }, rect },
  { node: { id: "b" }, rect: { left: 900, top: 430, right: 1220, bottom: 700 } },
]).state, "ambiguous", "two distinct right-rail anchors must fail closed");

const composer = new FakeElement();
const surface = new FakeElement();
composer.parentElement = surface;
composers = [composer];
assert.equal(context.__wakegptComposerCapability(false), "ready");
assert.equal(context.__wakegptComposerCapability(true), "ready");
assert.equal(context.__wakegptComposerAttachmentMode(), "dragDrop");
const dropPoint = context.__wakegptComposerDropPoint();
assert.equal(dropPoint?.x, 420);
assert.equal(dropPoint?.y, 650);
const firstInput = new FakeElement();
firstInput.accept = "image/*";
surface.fileInputs = [firstInput];
assert.equal(context.__wakegptComposerCapability(true), "ready");
assert.equal(context.__wakegptComposerAttachmentMode(), "fileInput");
assert.equal(context.__wakegptComposerImageInput(), firstInput);
const secondInput = new FakeElement();
secondInput.accept = "image/png";
surface.fileInputs = [firstInput, secondInput];
assert.equal(context.__wakegptComposerCapability(true), "imageInputAmbiguous");
assert.equal(context.__wakegptComposerImageInput(), null);

console.log("codex host contract: ok");
