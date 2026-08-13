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
  `globalThis.__wakegptCardTabVisibilityTest = {
    normalizeState,
    notebookTabOptions,
    workspaceSelectOptions,
    workspaceStateSignature,
    notebookStateSignature,
    captureInlineEdit,
    applyInlineEditSnapshot,
    canRestoreInlineEdit,
    notebookBlocksChanges,
    menuItemIndexAfterKey,
  };\n  ${mountMarker}`,
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

const notebookTabOptions = context.__wakegptCardTabVisibilityTest?.notebookTabOptions;
const normalizeState = context.__wakegptCardTabVisibilityTest?.normalizeState;
const workspaceSelectOptions = context.__wakegptCardTabVisibilityTest?.workspaceSelectOptions;
const workspaceStateSignature = context.__wakegptCardTabVisibilityTest?.workspaceStateSignature;
const notebookStateSignature = context.__wakegptCardTabVisibilityTest?.notebookStateSignature;
const captureInlineEdit = context.__wakegptCardTabVisibilityTest?.captureInlineEdit;
const applyInlineEditSnapshot = context.__wakegptCardTabVisibilityTest?.applyInlineEditSnapshot;
const canRestoreInlineEdit = context.__wakegptCardTabVisibilityTest?.canRestoreInlineEdit;
const notebookBlocksChanges = context.__wakegptCardTabVisibilityTest?.notebookBlocksChanges;
const menuItemIndexAfterKey = context.__wakegptCardTabVisibilityTest?.menuItemIndexAfterKey;
assert.equal(typeof notebookTabOptions, "function", "card must expose its notebook tab filter to the fixture");
assert.equal(typeof normalizeState, "function", "card must expose state normalization to the fixture");
assert.equal(typeof workspaceSelectOptions, "function", "card must expose its workspace option filter to the fixture");
assert.equal(typeof workspaceStateSignature, "function", "card must expose its workspace refresh signature");
assert.equal(typeof notebookStateSignature, "function", "card must expose its notebook refresh signature");
assert.equal(typeof captureInlineEdit, "function", "card must expose inline edit capture to the fixture");
assert.equal(typeof applyInlineEditSnapshot, "function", "card must expose inline edit restoration to the fixture");
assert.equal(typeof canRestoreInlineEdit, "function", "card must expose the inline edit restoration guard");
assert.equal(typeof notebookBlocksChanges, "function", "card must expose the notebook write guard");
assert.equal(typeof menuItemIndexAfterKey, "function", "card must expose its menu keyboard model");
assert.equal(menuItemIndexAfterKey(-1, 4, "ArrowDown"), 0);
assert.equal(menuItemIndexAfterKey(-1, 4, "ArrowUp"), 3);
assert.equal(menuItemIndexAfterKey(3, 4, "ArrowDown"), 0);
assert.equal(menuItemIndexAfterKey(0, 4, "ArrowUp"), 3);
assert.equal(menuItemIndexAfterKey(2, 4, "Home"), 0);
assert.equal(menuItemIndexAfterKey(1, 4, "End"), 3);
assert.equal(menuItemIndexAfterKey(1, 0, "ArrowDown"), -1);

assert.deepEqual(JSON.parse(JSON.stringify(workspaceSelectOptions([
  { id: "workspace", displayName: "产品工作区" },
  { id: "invalid", displayName: null },
]))), [{ value: "workspace", label: "产品工作区" }]);

const options = JSON.parse(JSON.stringify(notebookTabOptions([
  { id: "pinned", displayName: "产品记录", isPinned: true },
  { id: "closed", displayName: "测试", isPinned: false },
  { id: "invalid-name", displayName: null, isPinned: true },
])));
assert.deepEqual(options, [
  { value: "", label: "收件箱（仅本地）" },
  { value: "pinned", label: "产品记录" },
]);
const normalized = normalizeState({
  notebooks: [
    ...Array.from({ length: 24 }, (_, index) => ({
      id: `closed-${index}`,
      displayName: `已关闭 ${index}`,
      isPinned: false,
    })),
    { id: "late-pinned", displayName: "后置固定", isPinned: true },
  ],
});
assert.deepEqual(
  JSON.parse(JSON.stringify(notebookTabOptions(normalized.notebooks))),
  [
    { value: "", label: "收件箱（仅本地）" },
    { value: "late-pinned", label: "后置固定" },
  ],
  "closed tabs before a pinned tab must not consume the card's normalization bound",
);
const manyPinned = normalizeState({
  notebooks: Array.from({ length: 48 }, (_, index) => ({
    id: `pinned-${index}`,
    displayName: `固定 ${index + 1}`,
    isPinned: true,
  })),
});
const manyPinnedOptions = JSON.parse(JSON.stringify(notebookTabOptions(manyPinned.notebooks)));
assert.equal(manyPinnedOptions.length, 49, "the inbox plus all 48 card notebooks must remain selectable");
assert.deepEqual(manyPinnedOptions.at(-1), { value: "pinned-47", label: "固定 48" });

const workspaceStructure = [
  { id: "workspace-a", displayName: "产品" },
  { id: "workspace-b", displayName: "测试" },
];
const workspaceSignature = workspaceStateSignature(workspaceStructure);
for (const changedWorkspaces of [
  workspaceStructure.slice(0, 1),
  [...workspaceStructure].reverse(),
  [{ ...workspaceStructure[0], displayName: "产品记录" }, workspaceStructure[1]],
]) {
  assert.notEqual(
    workspaceStateSignature(changedWorkspaces),
    workspaceSignature,
    "workspace add/remove/order/name changes must invalidate the rendered card",
  );
}

const notebookStructure = [{
  id: "notebook-a",
  displayName: "产品记录",
  targetState: "ready",
  lastErrorCode: null,
  numberingStart: 1,
  numberingSyncPending: false,
  attachmentDirectory: "assets",
  attachmentDirectorySyncPending: false,
  isPinned: true,
}];
const notebookSignature = notebookStateSignature(notebookStructure);
for (const [field, value] of [
  ["displayName", "产品日志"],
  ["targetState", "conflict"],
  ["lastErrorCode", "notebook_moved_to_trash"],
  ["numberingStart", 9],
  ["numberingSyncPending", true],
  ["attachmentDirectory", "images"],
  ["attachmentDirectorySyncPending", true],
  ["isPinned", false],
]) {
  assert.notEqual(
    notebookStateSignature([{ ...notebookStructure[0], [field]: value }]),
    notebookSignature,
    `notebook ${field} changes must invalidate the rendered card`,
  );
}
assert.notEqual(
  notebookStateSignature([...notebookStructure, { ...notebookStructure[0], id: "notebook-b" }]),
  notebookSignature,
  "notebook add/remove changes must invalidate the rendered card",
);

const draftTextarea = { value: "未保存的正文", selectionStart: 2, selectionEnd: 6 };
const draftSave = { dataset: { revision: "17" } };
const draftEditor = {
  dataset: { mutationId: "mutation-a" },
  querySelector: (selector) => selector === "textarea" ? draftTextarea : selector === '[data-action="saveEdit"]' ? draftSave : null,
  closest: (selector) => selector === ".item" ? { dataset: { recordId: "record-a" } } : null,
};
const inlineSnapshot = captureInlineEdit(
  { querySelector: (selector) => selector === ".inline-edit" ? draftEditor : null },
  "workspace-a:notebook-a",
);
assert.deepEqual(JSON.parse(JSON.stringify(inlineSnapshot)), {
  selectionKey: "workspace-a:notebook-a",
  recordId: "record-a",
  bodyMarkdown: "未保存的正文",
  selectionStart: 2,
  selectionEnd: 6,
  expectedRevision: "17",
  mutationId: "mutation-a",
});
assert.equal(canRestoreInlineEdit(inlineSnapshot, {
  selectionKey: "workspace-a:notebook-a",
  recentRecords: [{ id: "record-a", revision: 17 }],
  selectedNotebookId: "notebook-a",
  notebooks: notebookStructure,
}), true);
assert.equal(canRestoreInlineEdit(inlineSnapshot, {
  selectionKey: "workspace-b:inbox",
  recentRecords: [{ id: "record-a", revision: 17 }],
  selectedNotebookId: null,
  notebooks: notebookStructure,
}), false, "navigation must not restore an edit into another selection");
assert.equal(canRestoreInlineEdit(inlineSnapshot, {
  selectionKey: "workspace-a:notebook-a",
  recentRecords: [],
  selectedNotebookId: "notebook-a",
  notebooks: notebookStructure,
}), false, "a deleted record must not be recreated by transient UI restoration");
assert.equal(canRestoreInlineEdit(inlineSnapshot, {
  selectionKey: "workspace-a:notebook-a",
  recentRecords: [{ id: "record-a", revision: 17 }],
  selectedNotebookId: "notebook-a",
  notebooks: [{ ...notebookStructure[0], targetState: "conflict" }],
}), false, "a conflict transition must discard stale inline edits");
assert.equal(notebookBlocksChanges({ ...notebookStructure[0], targetState: "conflict" }), true);
assert.equal(canRestoreInlineEdit(inlineSnapshot, {
  selectionKey: "workspace-a:notebook-a",
  recentRecords: [{ id: "record-a", revision: 18 }],
  selectedNotebookId: "notebook-a",
  notebooks: notebookStructure,
}), false, "a newer record revision must not receive the stale edit snapshot");

let restoredSelection = null;
let restoredFocused = false;
const restoredTextarea = {
  value: "server body",
  focus: () => { restoredFocused = true; },
  setSelectionRange: (start, end) => { restoredSelection = [start, end]; },
};
const restoredSave = { dataset: { revision: "99" } };
applyInlineEditSnapshot(restoredTextarea, restoredSave, inlineSnapshot);
assert.equal(restoredTextarea.value, "未保存的正文");
assert.equal(restoredSave.dataset.revision, "17", "refresh must retain the original optimistic revision");
assert.deepEqual(restoredSelection, [2, 6]);
assert.equal(restoredFocused, true);
assert.doesNotMatch(cardScript, /（未固定）/u, "closed tabs must not remain selectable in the card");
assert.match(
  cardScript,
  /notebookSelect\.setOptions\(\s*notebookOptions,/u,
  "the rendered notebook selector must use the pinned-tab filter",
);
assert.match(cardScript, /const closeTabButton = button\("close-tab", "", "unpinNotebook"\)/u);
assert.match(
  cardScript,
  /if \(action === "unpinNotebook"\) \{\s*send\("unpinNotebook", \{ bodyMarkdown: input\.value \}\);/u,
  "closing the active tab must keep sending the unpin action",
);
assert.match(cardScript, /const notebookOptions = notebookTabOptions\(state\.notebooks\)/u);
assert.match(cardScript, /\.slice\(0, 64\)/u, "Wake Select must retain the inbox plus all 48 notebooks");
assert.match(cardScript, /notebookOptions\.length > 1/u);
assert.match(cardScript, /当前没有已固定速记本/u);
assert.match(cardScript, /const selectionChanged = state\.selectionKey !== previousSelectionKey/u);
assert.match(cardScript, /const notebookStateChanged = notebookStateSignature\(state\.notebooks\)/u);
assert.match(cardScript, /const workspaceStateChanged = workspaceStateSignature\(state\.workspaces\)/u);
assert.match(cardScript, /!preserveTransientUi \|\| selectionChanged \|\| workspaceStateChanged \|\| notebookStateChanged/u);
assert.match(cardScript, /beginInlineEdit\(inlineEditSnapshot\.recordId/u);
assert.match(cardScript, /canRestoreInlineEdit\(inlineEditSnapshot, state\)/u);
assert.match(cardScript, /textarea\.readOnly = targetBlocked \|\| saving/u);
assert.match(cardScript, /const recordEditStale = Boolean/u);
assert.match(cardScript, /codex-26\.803\.81509-v41/u);
assert.match(adapterSource, /codex-26\.803\.81509-v41/u);
assert.ok(
  adapterSource.indexOf("let mut observed_refresh_generation = app.state::<CodexCardRefreshSignal>().current();")
    < adapterSource.indexOf("let initial_state = build_card_state(app, &mut session)?;"),
  "the card must sample refresh generation before building its initial state",
);
assert.match(adapterSource, /\.filter\(\|notebook\| notebook\.is_pinned\)[\s\S]*\.chain\(notebooks\.iter\(\)\.filter\(\|notebook\| !notebook\.is_pinned\)\)/u);

console.log("codex card tab visibility: ok");
