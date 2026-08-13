import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const [ui, app, bridge, preview, card, adapter, attachments] = await Promise.all([
  readFile(new URL("../src/ui.tsx", import.meta.url), "utf8"),
  readFile(new URL("../src/App.tsx", import.meta.url), "utf8"),
  readFile(new URL("../src/bridge.ts", import.meta.url), "utf8"),
  readFile(new URL("../src/preview.ts", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/assets/codex_card_v1.js", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/codex_adapter.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/attachments.rs", import.meta.url), "utf8"),
]);

assert.match(bridge, /invoke<ArrayBuffer>\("read_pending_image_preview"/u);
assert.match(bridge, /invoke<ArrayBuffer>\("read_record_image_preview"/u);
assert.match(attachments, /observe_expected_attachment\(/u);
assert.match(attachments, /sha256_hex\(&bytes\) != expected_sha256/u);
assert.match(attachments, /attachment_preview_media_type_changed/u);

assert.match(ui, /className="image-preview-button"/u);
assert.match(ui, /new IntersectionObserver/u);
assert.match(ui, /URL\.revokeObjectURL/u);
assert.match(ui, /role="dialog"/u);
assert.match(ui, /event\.key === "Escape"/u);
assert.match(ui, /editRetainedAttachmentIds/u);
assert.match(ui, /editPendingAttachments/u);
assert.match(ui, /onPaste=\{\(event\) => \{[\s\S]*stageEditImageFiles/u);
assert.match(ui, /保存后移除/u);
assert.match(bridge, /retainedAttachmentIds\?: string\[\]/u);
assert.match(bridge, /newAttachmentTokens\?: string\[\]/u);
assert.match(app, /retainedAttachmentIds,/u);
assert.match(app, /newAttachmentTokens: newAttachments\.map/u);
assert.match(preview, /record\.attachments = \[\.\.\.current, \.\.\.added\]/u);

assert.match(adapter, /"loadImagePreview" =>/u);
assert.match(adapter, /record\.notebook_id != session\.notebook_id/u);
assert.match(adapter, /read_record_preview\(&workspace, attachment\)/u);
assert.match(card, /__wakegptReceiveImage_/u);
assert.match(card, /new Uint8Array\(message\.byteSize\)/u);
assert.match(card, /\/\^\[0-9a-f\]\+\$\/u\.test\(message\.hex\)/u);
assert.match(card, /URL\.revokeObjectURL/u);
assert.match(card, /class="image-lightbox"|"image-lightbox"/u);

console.log("image preview contract: ok");
