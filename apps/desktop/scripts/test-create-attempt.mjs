import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const read = (relativePath) => readFile(new URL(relativePath, import.meta.url), "utf8");
const [app, card, adapter, commands, storage] = await Promise.all([
  read("../src/App.tsx"),
  read("../src-tauri/assets/codex_card_v1.js"),
  read("../src-tauri/src/codex_adapter.rs"),
  read("../src-tauri/src/commands.rs"),
  read("../src-tauri/src/storage.rs"),
]);

assert.match(app, /const bodyMarkdown = composer\.trimEnd\(\);/u);

assert.match(storage, /pub const SCHEMA_VERSION: u32 = 21;/u);
assert.match(storage, /CREATE TABLE IF NOT EXISTS draft_create_attempts/u);
assert.match(storage, /fn create_record_from_draft_attempt\(/u);
assert.match(storage, /saved_draft_fingerprint == draft_fingerprint/u);
assert.match(storage, /saved_request_sha256 == proposed\.request_sha256/u);
assert.match(storage, /DELETE FROM draft_create_attempts[\s\S]*draft_fingerprint <> \?3/u);
assert.match(storage, /lost_create_response_reuses_the_draft_attempt_across_new_request_ids/u);
assert.match(storage, /draft_attachment_identity_preserves_or_rotates_the_create_attempt/u);
assert.match(storage, /schema_thirteen_adds_durable_draft_attempts_without_rewriting_data/u);

assert.match(commands, /store\.create_record_from_draft_attempt\(/u);
assert.match(adapter, /store\.create_record_from_draft_attempt\(/u);
assert.match(adapter, /"ackCreateRecord" =>/u);
assert.match(adapter, /pending_create_ack/u);
assert.match(adapter, /CoordinatedMutationError::Saved/u);

assert.match(card, /let createRequestTimer = 0;/u);
assert.match(card, /let pendingCreateAck = null;/u);
assert.match(card, /let createAckTimer = 0;/u);
assert.match(card, /未确认记录结果；再次点击会安全核对同一提交/u);
assert.match(card, /pendingCreateAck = send\("ackCreateRecord", \{\}\)/u);
assert.match(card, /input\.value = state\.draftBodyMarkdown;/u);
assert.match(card, /pendingActions\.delete\(pendingCreate\)/u);
assert.doesNotMatch(card, /localStorage|sessionStorage/u);

console.log("durable create attempt contract: ok");
