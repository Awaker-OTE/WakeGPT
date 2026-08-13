import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";

const cardUrl = new URL("../src-tauri/assets/codex_card_v1.js", import.meta.url);
const adapterUrl = new URL("../src-tauri/src/codex_adapter.rs", import.meta.url);
const [cardScript, adapterSource] = await Promise.all([
  readFile(cardUrl, "utf8"),
  readFile(adapterUrl, "utf8"),
]);
const mountMarker = "globalThis.__wakegptMountCodexCard = mount;";
assert.ok(cardScript.includes(mountMarker), "card test hook marker is missing");

const instrumented = cardScript.replace(
  mountMarker,
  `globalThis.__wakegptImageInputTest = { imageFilesFromTransfer, transferContainsFiles };\n  ${mountMarker}`,
);
class FakeElement {}
const context = vm.createContext({
  console,
  document: { querySelectorAll: () => [], body: {} },
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

const collect = context.__wakegptImageInputTest?.imageFilesFromTransfer;
const containsFiles = context.__wakegptImageInputTest?.transferContainsFiles;
assert.equal(typeof collect, "function", "card must expose its clipboard image filter to the fixture");
assert.equal(typeof containsFiles, "function", "card must detect file drags before files are readable");
const png = { name: "clipboard.png", type: "image/png", size: 68 };
const svg = { name: "vector.svg", type: "image/svg+xml", size: 100 };
const fromFiles = collect({ files: [png, svg], items: [] });
assert.equal(fromFiles.length, 1);
assert.equal(fromFiles[0].name, "clipboard.png");

const gif = { name: "clipboard.gif", type: "image/gif", size: 80 };
const fromItems = collect({
  files: [],
  items: [
    { kind: "string", getAsFile: () => null },
    { kind: "file", getAsFile: () => gif },
  ],
});
assert.equal(fromItems.length, 1);
assert.equal(fromItems[0].type, "image/gif");
assert.equal(containsFiles({ types: ["Files"], files: [] }), true);
assert.equal(containsFiles({ types: ["text/plain"], files: [] }), false);

assert.match(cardScript, /imageInput\.addEventListener\("change"/u);
assert.match(cardScript, /input\.addEventListener\("paste"/u);
assert.match(cardScript, /composer\.addEventListener\("dragenter"/u);
assert.match(cardScript, /composer\.addEventListener\("dragover"/u);
assert.match(cardScript, /composer\.addEventListener\("dragleave"/u);
assert.match(
  cardScript,
  /composer\.addEventListener\("drop", \(event\) => \{[\s\S]*event\.preventDefault\(\);[\s\S]*imageFilesFromTransfer\(event\.dataTransfer\)[\s\S]*queueImageFiles\(files\);[\s\S]*\}\);/u,
);
assert.match(cardScript, /event\.dataTransfer\.dropEffect = canAcceptImageDrop\(\) \? "copy" : "none"/u);
assert.match(cardScript, /const imageDropOverlay = element\("div", "image-drop-overlay"\)/u);
assert.match(cardScript, /imageDropOverlay\.setAttribute\("role", "status"\)/u);
assert.match(cardScript, /松开以添加图片/u);
assert.match(
  cardScript,
  /\.image-drop-overlay \{[^}]*position:absolute;[^}]*pointer-events:none;/u,
);
assert.match(cardScript, /const queueImageFiles = \(files\) =>/u);
assert.doesNotMatch(cardScript, /send\("pickImages"/u);
assert.doesNotMatch(adapterSource, /"pickImages"\s*=>/u);
assert.match(cardScript, /editor\.dataset\.retainedAttachmentIds = JSON\.stringify/u);
assert.match(cardScript, /action === "toggleEditAttachment"/u);
assert.match(cardScript, /let recordEditSession = null/u);
assert.match(cardScript, /const queueRecordEditImageFiles = \(files, editor\) =>/u);
assert.match(cardScript, /textarea\.addEventListener\("paste", \(event\) => \{[\s\S]*queueRecordEditImageFiles/u);
assert.match(cardScript, /editor\.addEventListener\("drop", \(event\) => \{[\s\S]*queueRecordEditImageFiles/u);
assert.match(cardScript, /action === "addEditImages"[\s\S]*imageInput\.dataset\.editRecordId/u);
assert.match(cardScript, /action === "removeEditPendingImage"[\s\S]*removeRecordEditImage/u);
assert.match(cardScript, /send\("editRecord", payload, editSession\.mutationId\)/u);
assert.match(cardScript, /uploadId: editSession\.uploadId,[\s\S]*byteSize: entry\.file\.size/u);
assert.match(cardScript, /pendingRecordEditRequest = \{[\s\S]*mutationId: editSession\.mutationId/u);
assert.match(cardScript, /const inlineEditSnapshot = !message\.requestId \|\| completedRecordEdit/u);
assert.match(cardScript, /restoreCompletedRecordEdit[\s\S]*errorMessage: responseStatus/u);
assert.match(cardScript, /clearRecordEditSession\(\);[\s\S]*render\(\);/u);
assert.match(cardScript, /editor\.setAttribute\("aria-busy", String\(saving\)\)/u);
assert.match(cardScript, /toggle\.setAttribute\("aria-pressed"/u);
assert.match(cardScript, /inline-edit-image-state[\s\S]*待移除/u);
assert.match(adapterSource, /"editRecord" => handle_card_record_edit/u);
assert.match(adapterSource, /fn stage_card_image_uploads/u);
assert.match(adapterSource, /fn card_record_edit_plan/u);
assert.match(adapterSource, /current\.notebook_id != session\.notebook_id/u);
assert.match(
  adapterSource,
  /execute_record_mutation_with_attachments\([\s\S]*?\.materialize_locked\([\s\S]*?revise_record_attachment_set_idempotent/u,
  "card edit attachments must materialize while the composite mutation owns every lock",
);
assert.match(adapterSource, /revise_record_attachment_set_idempotent/u);
assert.match(adapterSource, /card_record_edit_saved_pending/u);
assert.match(adapterSource, /discard_card_staged_images\(&coordinator, &staged\)/u);
const editHandler = adapterSource.match(
  /fn handle_card_record_edit\([\s\S]*?\n\}\n\nfn handle_card_action/u,
)?.[0] || "";
assert.ok(editHandler, "the record edit handler must remain independently inspectable");
assert.doesNotMatch(editHandler, /session\.pending_attachments/u);
assert.doesNotMatch(editHandler, /save_card_draft/u);

console.log("codex card image input: ok");
