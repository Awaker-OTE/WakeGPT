import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createServer } from "vite";

const appCssUrl = new URL("../src/App.css", import.meta.url);
const appUiUrl = new URL("../src/ui.tsx", import.meta.url);
const cardUrl = new URL("../src-tauri/assets/codex_card_v1.js", import.meta.url);
const appUrl = new URL("../src/App.tsx", import.meta.url);
const [appCss, appUi, app, cardScript] = await Promise.all([
  readFile(appCssUrl, "utf8"),
  readFile(appUiUrl, "utf8"),
  readFile(appUrl, "utf8"),
  readFile(cardUrl, "utf8"),
]);

const server = await createServer({
  logLevel: "silent",
  server: { middlewareMode: true },
  appType: "custom",
});

try {
  const { menuItemIndexAfterKey, toolbarIndexAfterKey } = await server.ssrLoadModule("/src/ui.tsx");
  const enabled = [true, true, false, true];
  assert.equal(toolbarIndexAfterKey(0, enabled, "ArrowLeft"), 3);
  assert.equal(toolbarIndexAfterKey(3, enabled, "ArrowRight"), 0);
  assert.equal(toolbarIndexAfterKey(1, enabled, "ArrowRight"), 3);
  assert.equal(toolbarIndexAfterKey(3, enabled, "ArrowLeft"), 1);
  assert.equal(toolbarIndexAfterKey(3, enabled, "Home"), 0);
  assert.equal(toolbarIndexAfterKey(0, enabled, "End"), 3);
  assert.equal(toolbarIndexAfterKey(-1, enabled, "ArrowRight"), 0);
  assert.equal(toolbarIndexAfterKey(-1, enabled, "ArrowLeft"), 3);
  assert.equal(toolbarIndexAfterKey(1, [false, false], "ArrowRight"), -1);
  assert.equal(toolbarIndexAfterKey(1, enabled, "Enter"), 1);
  assert.equal(menuItemIndexAfterKey(-1, 3, "ArrowDown"), 0);
  assert.equal(menuItemIndexAfterKey(-1, 3, "ArrowUp"), 2);
  assert.equal(menuItemIndexAfterKey(2, 3, "ArrowDown"), 0);
  assert.equal(menuItemIndexAfterKey(0, 3, "ArrowUp"), 2);
  assert.equal(menuItemIndexAfterKey(1, 3, "Home"), 0);
  assert.equal(menuItemIndexAfterKey(1, 3, "End"), 2);
  assert.equal(menuItemIndexAfterKey(1, 0, "ArrowDown"), -1);
} finally {
  await server.close();
}

const ruleBody = (source, selector) => {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return source.match(new RegExp(`${escaped}\\s*\\{([^}]*)\\}`, "u"))?.[1] || "";
};

for (const selector of [
  ".composer-card textarea:focus-visible",
  ".document-editor > textarea:focus-visible",
]) {
  const body = ruleBody(appCss, selector);
  assert.ok(body, `${selector} must restore a visible keyboard focus indicator after outline resets`);
  assert.match(body, /outline:\s*2px\s+solid/u, `${selector} must use a real focus outline`);
  assert.ok(
    appCss.indexOf(`${selector} {`) > appCss.indexOf(selector.replace(":focus-visible", "")),
    `${selector} must appear after the base outline reset`,
  );
}

assert.match(appUi, /aria-label="新建速记"/u);
assert.match(appUi, /aria-label="Markdown 全文"/u);
assert.match(
  appUi,
  /className="composer-toolbar"[\s\S]*role="toolbar"[\s\S]*aria-orientation="horizontal"[\s\S]*onFocusCapture=\{handleToolbarFocus\}[\s\S]*onKeyDownCapture=\{handleToolbarKeyDown\}/u,
  "the shared main-app and quick-capture toolbar must expose horizontal toolbar semantics and roving focus handlers",
);
assert.match(
  appUi,
  /event\.key === "Tab" && workspaceMenuElementRef\.current\?\.contains\(document\.activeElement\)[\s\S]*event\.preventDefault\(\)[\s\S]*moveFocusFromWorkspaceMenu\(event\.shiftKey\)/u,
  "the workspace menu must close and move once past the trigger instead of exposing every menu item to Tab",
);
assert.equal(
  (appUi.match(/role="menuitem"\s+tabIndex=\{-1\}/gu) ?? []).length,
  3,
  "all workspace menu items must be programmatically focused composite children",
);
assert.match(
  cardScript,
  /itemMenu\.id = `\$\{hostId\}-record-menu`[\s\S]*menuButton\.setAttribute\("role", "menuitem"\)[\s\S]*menuButton\.tabIndex = -1/u,
  "the Codex record menu must own non-Tabbable menuitem children",
);
assert.match(
  cardScript,
  /trigger\.setAttribute\("aria-controls", itemMenu\.id\)[\s\S]*event\.key === "Tab" && menuHasFocus[\s\S]*moveFocusFromItemMenu\(event\.shiftKey\)/u,
  "the Codex record menu trigger must control the menu and Tab must leave it in one step",
);
assert.match(
  cardScript,
  /document\.querySelectorAll\(cardFocusableSelector\)[\s\S]*!control\.closest\(`#\$\{hostId\}`\)[\s\S]*Node\.DOCUMENT_POSITION_[\s\S]*host\.compareDocumentPosition\(control\)[\s\S]*pageFocusable\[pageFocusable\.length - 1\][\s\S]*pageFocusable\[0\]/u,
  "the last or first record menu must continue into the surrounding ChatGPT page instead of trapping focus on its trigger",
);
assert.match(
  cardScript,
  /handleItemMenuTriggerKeyDown[\s\S]*event\.preventDefault\(\)[\s\S]*event\.stopPropagation\(\)[\s\S]*openItemMenu/u,
  "the opening arrow key must not immediately advance the newly opened menu a second time",
);
assert.match(
  cardScript,
  /recent\.addEventListener\("keydown", handleItemMenuTriggerKeyDown\)[\s\S]*recent\.removeEventListener\("keydown", handleItemMenuTriggerKeyDown\)/u,
  "Codex menu-button keyboard listeners must be removed with the card mount",
);
assert.match(
  appUi,
  /toolbarEnabledItems[\s\S]*toolbarEntryIndex[\s\S]*tabIndex=\{toolbarTabIndex\(0\)\}[\s\S]*tabIndex=\{toolbarTabIndex\(7\)\}/u,
  "the Markdown toolbar must expose exactly one enabled tab stop across its select and buttons",
);
assert.match(
  appUi,
  /toolbarControlElements\(event\.currentTarget, true\)[\s\S]*event\.target\.getAttribute\("aria-expanded"\) === "true"[\s\S]*toolbarIndexAfterKey[\s\S]*nextControl\?\.focus/u,
  "an open block-style listbox must keep its own keys while a closed toolbar uses arrow, Home, and End navigation",
);
assert.match(
  appUi,
  /toolbarControlIsVisible[\s\S]*style\.display !== "none"[\s\S]*style\.visibility !== "hidden"[\s\S]*focusedControl\.disabled \|\| !toolbarControlIsVisible\(focusedControl\)/u,
  "compact surfaces and newly disabled controls must be skipped and must return focus to an available toolbar item",
);
assert.match(
  appUi,
  /className="segmented-control" role="tablist" aria-label="Markdown 视图"[\s\S]*role="tab"[\s\S]*aria-controls=\{`\$\{markdownViewId\}-panel`\}[\s\S]*tabIndex=\{mode === view\.value \? 0 : -1\}[\s\S]*handleMarkdownViewKeyDown/u,
  "Markdown view tabs must use roving focus and the shared keyboard model",
);
assert.match(
  appUi,
  /className="document-editor"[\s\S]*role="tabpanel"[\s\S]*aria-labelledby=\{`\$\{markdownViewId\}-\$\{mode\}-tab`\}[\s\S]*tabIndex=\{!readyDocument \|\| mode === "preview" \? 0 : undefined\}/u,
  "the Markdown view panel must be labelled by the active tab and focusable when it has no editor",
);
assert.match(
  appUi,
  /className="large-markdown-edit-pagination" role="group" aria-label="大文档编辑分页"[\s\S]*<span id=\{largeEditStatusId\} role="status">[\s\S]*编辑第 \{largePageIndex \+ 1\}/u,
  "large-document editing pages must expose a named group and current-page status",
);
assert.match(
  appUi,
  /className="markdown-preview-pagination" role="group" aria-label="大文档预览分页"[\s\S]*<span role="status">[\s\S]*第 \{safePreviewPageIndex \+ 1\}/u,
  "large-document preview pages must expose a named group and current-page status",
);
assert.match(appUi, /integrationRestartNotice\.tone === "error" \? "alert" : "status"/u);
assert.match(appUi, /disabled=\{integrationBusy\}/u);
assert.match(
  appUi,
  /<DialogFrame title="本地诊断"[\s\S]*?role="list" aria-label="本地诊断事件"/u,
  "the diagnostics dialog must have a labelled dialog and event-list name",
);
assert.match(
  appUi,
  /<progress[\s\S]*aria-label=\{updateStatus\.phase === "verifying" \? "正在验证更新签名" : "更新下载进度"\}/u,
  "update download and signature verification must expose native progress semantics",
);
assert.match(
  appUi,
  /<DialogFrame title=\{title\} descriptionId=\{descriptionId\} closeDisabled=\{busy\}/u,
  "confirmation dialogs must describe their consequence and lock dismissal while committing",
);
assert.match(
  appUi,
  /event\.key === "Escape"[\s\S]*expandedControlInsideDialog\(dialog\)[\s\S]*if \(closeDisabledRef\.current\) return;[\s\S]*closeRef\.current\(\)/u,
  "dialog Escape must defer to an expanded control and refuse dismissal during a committed action",
);
assert.match(
  app,
  /title=\{`重启并更新到 WakeGPT \$\{updateStatus\?\.availableVersion \?\? ""\}？`\}[\s\S]*confirmLabel="保存并重启更新"/u,
  "self-update must use a separately named execution-time confirmation",
);
assert.match(
  appUi,
  /diagnostics-dialog__error" role="alert"/u,
  "diagnostics loading failures must be announced as alerts",
);
assert.match(
  appUi,
  /diagnostics-dialog__empty" aria-live="polite"/u,
  "diagnostics empty and loading states must be announced politely",
);
assert.match(
  app,
  /className="view-heading__identity"[\s\S]*?<p className="eyebrow" title=\{activeWorkspace\.displayName\}>[\s\S]*?<h1 title=\{activeNotebook\?\.displayName \?\? "收件箱"\}>/u,
  "bounded workspace and notebook labels must expose their complete text",
);
assert.match(
  app,
  /className="target-status"[\s\S]*?aria-label=\{activeNotebookTargetText\}[\s\S]*?title=\{activeNotebookTargetText\}[\s\S]*?className="target-status__text" aria-hidden="true"/u,
  "the bounded sync status must keep one complete accessible name",
);
assert.match(
  appUi,
  /<h2 id=\{titleId\} title=\{title\}>\{title\}<\/h2>/u,
  "bounded dialog titles must expose the complete title string",
);
assert.match(
  appUi,
  /className="conflict-inspector__path" title=\{notebook\.relativePath\}/u,
  "the wrapped conflict path must retain its complete native tooltip",
);
assert.match(
  cardScript,
  /targetHint\.title = targetHint\.textContent;/u,
  "the bounded card target hint must retain its complete text as a native tooltip",
);
assert.match(
  appUi,
  /role="switch"\s+aria-label="记录本地诊断"\s+aria-checked=\{localDiagnosticsStatus\?\.enabled \?\? false\}/u,
  "the diagnostics setting must expose its switch name and checked state",
);
assert.match(
  appUi,
  /localDiagnosticsBusy \? <div className="sr-only" role="status">正在更新本地诊断<\/div>/u,
  "diagnostics mutations must expose a non-visual live status",
);
assert.match(
  app,
  /<ConfirmDialog\s+open=\{localDiagnosticsClearOpen\}\s+title="清空本地诊断？"\s+confirmLabel="清空诊断"\s+busy=\{busyAction === "diagnostics-clear"\}\s+destructive/u,
  "clearing diagnostics must use a separately named destructive confirmation state",
);
assert.match(
  appUi,
  /<DialogFrame title="清除 WakeGPT 本机数据？"[\s\S]*aria-describedby="local-data-reset-confirmation-help"[\s\S]*disabled=\{busy \|\| !matches\}/u,
  "the whole-app reset must use a labelled dialog, described typed confirmation, and guarded destructive action",
);
assert.match(appCss, /@media \(prefers-reduced-motion:\s*reduce\)[\s\S]*?transition-duration:\s*0\.01ms\s*!important/u);
assert.match(
  appCss,
  /@media \(forced-colors: active\)[\s\S]*\.notebook-tab\[aria-selected="true"\], \.segmented-control \[aria-selected="true"\][\s\S]*forced-color-adjust: none/u,
  "both tab families must preserve a selected state in forced-colors mode",
);

const cardReducedMotion = cardScript.match(
  /@media \(prefers-reduced-motion:reduce\) \{([\s\S]*?)@media \(forced-colors:active\)/u,
)?.[1] || "";
assert.match(
  cardReducedMotion,
  /transition-duration:0\.01ms !important/u,
  "the Codex card must suppress transitions when reduced motion is requested",
);
assert.match(
  cardReducedMotion,
  /animation-duration:0\.01ms !important/u,
  "the Codex card must suppress animations when reduced motion is requested",
);
assert.match(cardScript, /button:focus-visible, input:focus-visible, textarea:focus-visible/u);
assert.match(cardScript, /editor\.setAttribute\("role", "group"\)/u);
assert.match(cardScript, /editor\.setAttribute\("aria-label", "编辑记录正文和图片"\)/u);
assert.match(cardScript, /gallery\.setAttribute\("aria-label", "当前与待添加图片"\)/u);
assert.match(cardScript, /toggle\.setAttribute\("aria-pressed"/u);
assert.match(cardScript, /dropOverlay\.setAttribute\("role", "status"\)/u);
assert.match(cardScript, /error\.setAttribute\("role", "alert"\)/u);
assert.match(cardScript, /editor\.setAttribute\("aria-busy", String\(saving\)\)/u);
assert.match(cardScript, /@media \(forced-colors:active\)[\s\S]*\.inline-edit-image-state/u);
assert.match(cardScript, /const handleInlineEditKeyDown = \(event\) =>/u);
assert.match(
  appUi,
  /const editTriggerRef = useRef<HTMLButtonElement \| null>\(null\)[\s\S]*trigger\?\.isConnected[\s\S]*trigger\.focus\(\{ preventScroll: true \}\)/u,
  "main-app record editing must restore focus only to a still-usable edit trigger",
);
assert.match(
  appUi,
  /className="record-edit"[\s\S]*role="group"[\s\S]*aria-label="编辑记录正文和图片"[\s\S]*event\.key !== "Escape"[\s\S]*editImageBusy[\s\S]*busyRecordId === record\.id[\s\S]*closeEdit\(\)/u,
  "Escape must cancel an idle main-app inline edit without interrupting an active image or save operation",
);
assert.match(
  appUi,
  /resetEditState\(false, true\)[\s\S]*onClick=\{\(event\) => beginEdit\(record, event\.currentTarget\)\}/u,
  "cancel and successful save paths must preserve the original edit trigger for focus restoration",
);

console.log("accessibility contract: ok");
