import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";

const cardUrl = new URL("../src-tauri/assets/codex_card_v1.js", import.meta.url);
const cardScript = await readFile(cardUrl, "utf8");
const mountMarker = "globalThis.__wakegptMountCodexCard = mount;";
assert.ok(cardScript.includes(mountMarker), "card test hook marker is missing");
const instrumentedCardScript = cardScript.replace(
  mountMarker,
  `globalThis.__wakegptComposerInsertionTest = {
    normalizeState,
    composerInsertionNeedsReview,
    composerActionExpired,
  };\n  ${mountMarker}`,
);

let activeComposers = [];
let focusedComposer = null;
const dispatchedEvents = [];

class FakeElement {
  constructor() {
    this.isContentEditable = false;
    this.innerHTML = "";
    this.textContent = "";
  }

  getBoundingClientRect() {
    return { width: 640, height: 120, bottom: 700 };
  }

  closest() {
    return null;
  }

  focus() {
    focusedComposer = this;
  }

  dispatchEvent(event) {
    dispatchedEvents.push(event);
    return true;
  }
}

class FakeTextAreaElement extends FakeElement {
  #value = "";

  get value() {
    return this.#value;
  }

  set value(next) {
    this.#value = String(next);
  }
}

class FakeInputEvent {
  constructor(type, init = {}) {
    this.type = type;
    this.bubbles = Boolean(init.bubbles);
    this.inputType = init.inputType;
    this.data = init.data;
  }
}

const selection = {
  removeAllRanges() {},
  addRange() {},
};

const context = vm.createContext({
  console,
  crypto: { randomUUID: () => "00000000-0000-4000-8000-000000000000" },
  document: {
    body: {},
    querySelectorAll: () => activeComposers,
    createRange: () => ({ selectNodeContents() {}, collapse() {} }),
    execCommand: (command, _showUi, value) => {
      assert.equal(command, "insertText");
      activeComposers[0].textContent += value;
      activeComposers[0].innerHTML += value;
      return true;
    },
  },
  globalThis: null,
  HTMLElement: FakeElement,
  HTMLTextAreaElement: FakeTextAreaElement,
  InputEvent: FakeInputEvent,
  MutationObserver: class {},
  ResizeObserver: class {},
  window: {
    innerWidth: 1280,
    innerHeight: 720,
    getSelection: () => selection,
  },
  getComputedStyle: () => ({ display: "block", visibility: "visible" }),
});
context.globalThis = context;
vm.runInContext(instrumentedCardScript, context, { filename: "codex_card_v1.js" });

const insert = context.__wakegptInsertIntoComposer;
const capability = context.__wakegptComposerCapability;
const normalizeState = context.__wakegptComposerInsertionTest?.normalizeState;
const insertionNeedsReview = context.__wakegptComposerInsertionTest?.composerInsertionNeedsReview;
const actionExpired = context.__wakegptComposerInsertionTest?.composerActionExpired;
assert.equal(typeof insert, "function", "the real card composer insertion hook must be available");
assert.equal(typeof capability, "function", "the composer capability probe must be available");
assert.equal(typeof normalizeState, "function", "the card state normalizer must be testable");
assert.equal(typeof insertionNeedsReview, "function");
assert.equal(typeof actionExpired, "function");
assert.equal(insertionNeedsReview("uncertain"), true);
assert.equal(insertionNeedsReview("staleUncertain"), true);
assert.equal(insertionNeedsReview("partial"), false);
assert.equal(actionExpired(1_000, 10_999), false);
assert.equal(actionExpired(1_000, 11_000), true);
assert.equal(actionExpired(12_000, 11_000), false);

const insertInto = (composer, value) => {
  activeComposers = [composer];
  focusedComposer = null;
  dispatchedEvents.length = 0;
  assert.equal(insert(value), true);
  assert.equal(focusedComposer, composer, "insertion must focus the host composer");
  assert.deepEqual(
    dispatchedEvents.map((event) => event.type),
    ["input"],
    "insertion must update the composer without dispatching submit or send events",
  );
};

const textarea = new FakeTextAreaElement();
textarea.value = "已有内容";
insertInto(textarea, "新 Markdown");
assert.equal(textarea.value, "已有内容\n\n新 Markdown");

const textareaWithNewline = new FakeTextAreaElement();
textareaWithNewline.value = "已有内容\n";
insertInto(textareaWithNewline, "新 Markdown");
assert.equal(textareaWithNewline.value, "已有内容\n\n新 Markdown");

const textareaWithBlankLine = new FakeTextAreaElement();
textareaWithBlankLine.value = "已有内容\n\n";
insertInto(textareaWithBlankLine, "新 Markdown");
assert.equal(textareaWithBlankLine.value, "已有内容\n\n新 Markdown");

const emptyTextarea = new FakeTextAreaElement();
insertInto(emptyTextarea, "新 Markdown");
assert.equal(emptyTextarea.value, "新 Markdown");

const contentEditable = new FakeElement();
contentEditable.isContentEditable = true;
contentEditable.textContent = "已有内容";
contentEditable.innerHTML = "已有内容";
insertInto(contentEditable, "新 Markdown");
assert.equal(contentEditable.textContent, "已有内容\n\n新 Markdown");

const ambiguousA = new FakeTextAreaElement();
const ambiguousB = new FakeTextAreaElement();
ambiguousA.value = "A";
ambiguousB.value = "B";
activeComposers = [ambiguousA, ambiguousB];
focusedComposer = null;
dispatchedEvents.length = 0;
assert.equal(capability(false), "composerAmbiguous");
assert.equal(insert("不应写入"), false, "ambiguous composers must fail closed");
assert.equal(ambiguousA.value, "A");
assert.equal(ambiguousB.value, "B");
assert.equal(focusedComposer, null);
assert.deepEqual(dispatchedEvents, []);

for (const insertionState of ["partial", "uncertain", "staleUncertain", "complete"]) {
  const normalized = normalizeState({
    recentRecords: [{ id: "record-a", insertionState }],
  });
  assert.equal(
    normalized.recentRecords[0].insertionState,
    insertionState,
    `${insertionState} composer receipts must survive card state normalization`,
  );
}
assert.equal(
  normalizeState({ recentRecords: [{ id: "record-a", insertionState: "invalid" }] })
    .recentRecords[0].insertionState,
  "none",
  "unknown composer receipt states must fail closed",
);

assert.match(cardScript, /const composerDocumentIdentity = \(\) =>/u);
assert.match(cardScript, /globalThis\.performance\?\.timeOrigin/u);
assert.match(cardScript, /const current = composerDocumentIdentity\(\)/u);
assert.doesNotMatch(
  cardScript,
  /observedComposerContext[\s\S]{0,320}document\.title/u,
  "a mutable page title must not orphan a composer receipt",
);

assert.match(cardScript, /record\.insertionState === "uncertain"\s*\? "核对插入"/u);
assert.match(cardScript, /record\.insertionState === "staleUncertain"\s*\? "核对已修改记录"/u);
assert.match(cardScript, /记录已在上次插入后修改/u);
assert.match(cardScript, /当前版本已看到/u);
assert.match(cardScript, /没有，插入当前版本/u);
assert.match(cardScript, /const pendingComposerInsertions = new Map\(\)/u);
assert.match(cardScript, /pendingComposerInsertions\.set\(requestId, \{ recordId, startedAt: Date\.now\(\) \}\)/u);
assert.match(cardScript, /pendingComposerResolution = \{ requestId, recordId, resolution, startedAt: Date\.now\(\) \}/u);
assert.match(cardScript, /const composerActionTimeoutMs = 10000/u);
assert.match(cardScript, /const reconcileComposerActionTimeouts = \(now = Date\.now\(\)\) =>/u);
assert.match(cardScript, /pendingActions\.delete\(requestId\);[\s\S]*已按当前耐久回执恢复，不会自动重试/u);
assert.match(cardScript, /reconcileComposerActionTimeouts\(now\)/u);
assert.match(cardScript, /heartbeatAge > 15000/u);
assert.match(
  cardScript,
  /completedComposerInsertionUncertain[\s\S]*activeComposerUncertaintyRecordId = completedComposerInsertionRecordId/u,
  "an uncertain insert response must open the inline review without requiring a second click",
);
assert.match(
  cardScript,
  /const uncertaintyRemains = state\.recentRecords\.some[\s\S]*activeComposerUncertaintyRecordId = completedComposerResolution\.recordId/u,
  "a retry that is still uncertain must keep the review open",
);
assert.match(cardScript, /setAttribute\("role", "group"\)/u);
assert.match(cardScript, /setAttribute\("aria-label", "核对插入结果"\)/u);
assert.match(cardScript, /请先查看 ChatGPT 输入框/u);
assert.match(cardScript, /WakeGPT 不会自动重试，以免重复插入/u);
assert.match(cardScript, /setLabeledButton\(confirm, "check", stale \? "当前版本已看到" : "已看到"\)/u);
assert.match(cardScript, /setLabeledButton\(retry, "refresh", stale \? "没有，插入当前版本" : "没有，重试"\)/u);
assert.match(cardScript, /confirm\.dataset\.resolution = "confirm"/u);
assert.match(cardScript, /retry\.dataset\.resolution = "retry"/u);
assert.match(cardScript, /button\("", "取消", "cancelComposerInsertionReview"\)/u);
assert.match(
  cardScript,
  /send\("resolveComposerInsertion", \{ recordId, resolution \}, requestId\)/u,
  "both explicit decisions must use the dedicated resolution action",
);
assert.match(cardScript, /if \(pendingComposerResolution \|\| !\["confirm", "retry"\]\.includes\(resolution\)\) return/u);
assert.match(cardScript, /confirm\.disabled = resolving;\s*retry\.disabled = resolving;\s*cancel\.disabled = resolving;/u);
assert.match(
  cardScript,
  /event\.defaultPrevented[\s\S]*event\.key !== "Escape"[\s\S]*closeComposerInsertionReview\(true\)/u,
  "an Escape already consumed by a topmost menu, select, or lightbox must not also close the review",
);
assert.match(
  cardScript,
  /const handlePanelKeyDown = \(event\) => \{[\s\S]*!event\.defaultPrevented[\s\S]*setExpanded\(false\)/u,
  "an Escape consumed by a nested surface must not also collapse the card drawer",
);
assert.match(cardScript, /window\.removeEventListener\("keydown", handleComposerInsertionReviewKeyDown\)/u);
assert.match(cardScript, /composerUncertaintyInvalidated/u);
assert.match(
  cardScript,
  /activeComposerUncertaintyRecordId = null;\s*pendingComposerResolution = null;\s*pendingComposerInsertions\.clear\(\);[\s\S]*observer\.disconnect\(\)/u,
);

assert.doesNotMatch(cardScript, /requestSubmit|form\.submit|dispatchEvent\(new Event\(["']submit/u);

console.log("composer insertion contract: ok");
