import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createServer } from "vite";

const appSource = await readFile(new URL("../src/App.tsx", import.meta.url), "utf8");
const appCss = await readFile(new URL("../src/App.css", import.meta.url), "utf8");
const server = await createServer({
  logLevel: "silent",
  server: { middlewareMode: true },
  appType: "custom",
});

try {
  const {
    activeNotebookAfterTabRefresh,
    tabIndexAfterKey,
    tabScrollLeftToReveal,
  } = await server.ssrLoadModule("/src/ui.tsx");
  const uiSource = await readFile(new URL("../src/ui.tsx", import.meta.url), "utf8");

  assert.equal(tabScrollLeftToReveal(0, 240, 852, 852, 1040), 188);
  assert.equal(tabScrollLeftToReveal(188, 240, 852, 100, 288), 48);
  assert.equal(tabScrollLeftToReveal(64, 240, 852, 300, 500), 64);
  assert.equal(tabScrollLeftToReveal(12, 240, 852, 120, 900), 0);

  assert.equal(tabIndexAfterKey(0, 4, "ArrowLeft"), 3);
  assert.equal(tabIndexAfterKey(3, 4, "ArrowRight"), 0);
  assert.equal(tabIndexAfterKey(2, 4, "Home"), 0);
  assert.equal(tabIndexAfterKey(1, 4, "End"), 3);
  assert.equal(tabIndexAfterKey(2, 4, "Enter"), 2);
  assert.equal(tabIndexAfterKey(-1, 4, "ArrowRight"), -1);
  assert.equal(tabIndexAfterKey(0, 0, "ArrowRight"), 0);

  assert.equal(activeNotebookAfterTabRefresh("pinned", [
    { id: "pinned", isPinned: true },
    { id: "closed", isPinned: false },
  ]), "pinned");
  assert.equal(activeNotebookAfterTabRefresh("closed", [
    { id: "pinned", isPinned: true },
    { id: "closed", isPinned: false },
  ]), null);
  assert.equal(activeNotebookAfterTabRefresh("missing", [
    { id: "pinned", isPinned: true },
  ]), null);
  assert.equal(activeNotebookAfterTabRefresh(null, [
    { id: "pinned", isPinned: true },
  ]), null);

  assert.match(
    uiSource,
    /role=\{disabled \? undefined : "tablist"\}[\s\S]*aria-orientation=\{disabled \? undefined : "horizontal"\}/u,
    "notebook tabs must expose a horizontal tablist only while their panel is visible",
  );
  assert.match(
    uiSource,
    /role=\{disabled \? undefined : "tab"\}[\s\S]*aria-controls=\{disabled \? undefined : "wakegpt-notes-panel"\}[\s\S]*tabIndex=\{disabled \? 0 : focusedTabIndex === 0 \? 0 : -1\}/u,
    "the inbox tab must use roving focus and control the notes panel",
  );
  assert.match(
    uiSource,
    /handleTabKeyDown\(event, notebook\.id\)/u,
    "bound notebook tabs must use the shared arrow, Home, and End key model",
  );
  assert.match(
    uiSource,
    /event\.key === "Delete" && notebookId[\s\S]*fallbackId = notebookId === activeNotebookId \? null : activeNotebookId[\s\S]*focusTabAtIndex\([\s\S]*onPinChange\(notebookId, false\)/u,
    "Delete must close a bound tab and return focus to the selected fallback",
  );
  assert.match(
    uiSource,
    /aria-label=\{notebookTabLabel\(notebook\)\}[\s\S]*aria-keyshortcuts="Alt\+ArrowLeft Alt\+ArrowRight Delete"/u,
    "bound tabs must expose their sync state and supported keyboard actions",
  );
  assert.match(
    uiSource,
    /className="notebook-tab-shell"[\s\S]*role=\{disabled \? undefined : "presentation"\}/u,
    "tab shells must not interrupt the tablist ownership tree",
  );
  assert.match(uiSource, /className="notebook-tab-label">\{notebook\.displayName\}/u);
  assert.match(
    appCss,
    /\.notebook-tab-label \{[^}]*min-width:\s*0;[^}]*overflow:\s*hidden;[^}]*text-overflow:\s*ellipsis;/u,
    "long notebook labels must still truncate after the close affordance moved inside the tab",
  );
  assert.match(
    uiSource,
    /className="notebook-tab-close"[\s\S]*title=\{`关闭 \$\{notebook\.displayName\} Tab（保留速记本）`\}[\s\S]*aria-hidden="true"/u,
  );
  assert.doesNotMatch(
    uiSource,
    /<button\s+className="notebook-tab-close"/u,
    "the visual close affordance must stay inside the single tab control",
  );
  const tabKeyHandler = uiSource.slice(
    uiSource.indexOf("const handleTabKeyDown ="),
    uiSource.indexOf("return (", uiSource.indexOf("const handleTabKeyDown =")),
  );
  assert.doesNotMatch(
    tabKeyHandler,
    /onSelect\(nextId\)/u,
    "arrow navigation must move focus without racing asynchronous notebook selection",
  );
  assert.ok(
    tabKeyHandler.indexOf('event.key === "Delete"')
      < tabKeyHandler.indexOf("if (disabled || event.altKey) return;"),
    "Delete must preserve keyboard access to close while notebook tabs act as navigation buttons",
  );
  assert.match(
    tabKeyHandler,
    /event\.key === "Enter" \|\| event\.key === " "[\s\S]*event\.preventDefault\(\)[\s\S]*onSelect\(notebookId\)/u,
    "focused notebook tabs must activate explicitly with Enter or Space",
  );
  assert.match(
    appSource,
    /id="wakegpt-notes-panel"[\s\S]*role="tabpanel"[\s\S]*aria-labelledby=\{activeNotebookId/u,
    "the visible notes view must be labelled by its active notebook tab",
  );

  const notebookRefreshStart = appSource.indexOf('if (change.changeKind === "notebooks") {');
  const notebookRefreshEnd = appSource.indexOf('if (change.notebookId === activeNotebookId)', notebookRefreshStart);
  assert.ok(notebookRefreshStart >= 0 && notebookRefreshEnd > notebookRefreshStart);
  const notebookRefresh = appSource.slice(notebookRefreshStart, notebookRefreshEnd);
  assert.match(notebookRefresh, /activeNotebookAfterTabRefresh\(/u);
  assert.match(notebookRefresh, /activeWorkspaceIdRef\.current !== workspaceId/u);
  assert.match(notebookRefresh, /activeNotebookIdRef\.current !== activeNotebookAtEvent/u);
  assert.match(notebookRefresh, /await wakeBridge\.saveDraft\(\{/u);
  assert.match(notebookRefresh, /setActiveNotebookId\(\(current\) =>/u);
  assert.match(
    notebookRefresh,
    /await loadWorkspaceRecords\(\s*workspaceId,\s*nextActiveNotebookId,\s*view,\s*nextNotebooks,\s*\)/u,
  );
  assert.ok(
    notebookRefresh.indexOf("await wakeBridge.saveDraft({")
      < notebookRefresh.indexOf("setNotebooks(nextNotebooks)"),
    "a remote close must save the active draft before replacing the visible tab state",
  );

  console.log("notebook active-tab visibility: ok");
} finally {
  await server.close();
}
