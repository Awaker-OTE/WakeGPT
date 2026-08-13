import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";

const [card, appCss] = await Promise.all([
  readFile(new URL("../src-tauri/assets/codex_card_v1.js", import.meta.url), "utf8"),
  readFile(new URL("../src/App.css", import.meta.url), "utf8"),
]);

assert.match(card, /const cardScroll = element\("div", "card-scroll"\)/u);
assert.match(card, /const appLauncherButton = button\("icon app-launcher", "", "openApp"\)/u);
assert.match(card, /setIconButton\(appLauncherButton, "external", "打开 WakeGPT App"\)/u);
assert.match(card, /header\.append\(appLauncherButton, expandButton, notebookMenuButton\)/u);
assert.match(card, /cardScroll\.append\(selectors, targetHint, createPanel, composer, recentHeading, recent\)/u);
assert.match(card, /card\.append\(header, cardScroll\)/u);
assert.doesNotMatch(card, /button\("open-app", "", "openApp"\)/u);
assert.match(card, /\.card-scroll \{[^}]*min-height:0;[^}]*overflow-y:auto;[^}]*scrollbar-gutter:stable;/u);
assert.match(card, /\.image-preview img \{[^}]*object-fit:\s*contain;/u);
assert.match(appCss, /\.image-preview-button img \{[^}]*object-fit:\s*contain;/u);
assert.match(
  card,
  /\.icon \{[^}]*position:relative;[^}]*flex:0 0 34px;[^}]*width:34px;[^}]*height:34px;[^}]*min-height:34px;[^}]*padding:0;[^}]*place-items:center;[^}]*border-color:transparent;/u,
  "Codex card icon controls must expose a real 34px pointer surface",
);
assert.match(
  card,
  /\.icon::before \{[^}]*content:"";[^}]*position:absolute;[^}]*inset:4px;[^}]*pointer-events:none;[^}]*border:1px solid/u,
  "the visible icon surface must stay compact without faking the hit target",
);
assert.match(
  card,
  /button \{[^}]*-webkit-app-region:no-drag;[^}]*touch-action:manipulation;/u,
  "all card buttons must stay interactive inside Electron's top application region",
);
assert.match(
  card,
  /\.lucide \{[^}]*pointer-events:none;/u,
  "icon paths must not split the button pointer target",
);
assert.match(
  appCss,
  /\.icon-button, \.composer-toolbar > button, \.record-actions button, \.toast button \{[^}]*width:\s*30px;[^}]*height:\s*30px;[^}]*padding:\s*0;[^}]*place-items:\s*center;/u,
  "desktop icon controls must keep an explicit full-button hit surface",
);
assert.doesNotMatch(card, /:host\(\[data-layout="full"\]\) \.expand-toggle \{ display:none; \}/u);
assert.match(card, /setIconButton\(expandButton, "minimize", "收起速记面板"\)/u);
assert.match(card, /const drawerTop = Math\.max\(48, Math\.round\(rect\.top\)\)/u);
assert.match(card, /host\.style\.top = `\$\{drawerTop\}px`;/u);
assert.match(card, /host\.style\.height = `\$\{Math\.max\(240, window\.innerHeight - drawerTop - 14\)\}px`;/u);
assert.match(card, /event\.key === "Escape" && expandedOverlay/u);
assert.match(
  card,
  /textarea \{[^}]*min-height:max\(72px,5\.2em\);[^}]*max-height:max\(150px,10em\);/u,
  "the full card composer must grow with 200% text instead of clipping to a fixed pixel box",
);
assert.match(
  card,
  /:host\(\[data-layout="compact"\]\) \.composer textarea \{[^}]*min-height:max\(50px,5\.2em\);[^}]*max-height:max\(76px,7em\);/u,
  "the compact card composer must retain a text-relative input height",
);

const mountMarker = "const mount = (bootstrap) => {";
const instrumented = card.replace(
  mountMarker,
  `globalThis.__wakegptCardLayoutTest = { collapsedCardMetrics };\n  ${mountMarker}`,
);
assert.notEqual(instrumented, card, "card layout helper must be instrumentable");
const context = vm.createContext({});
vm.runInContext(instrumented, context, { filename: "codex_card_v1.js" });
const collapsedCardMetrics = context.__wakegptCardLayoutTest?.collapsedCardMetrics;
assert.equal(typeof collapsedCardMetrics, "function");
assert.deepEqual(
  { ...collapsedCardMetrics(320, 13) },
  { width: 154, height: 47 },
  "ordinary text must preserve the compact collapsed footprint",
);
assert.deepEqual(
  { ...collapsedCardMetrics(320, 26) },
  { width: 189, height: 67 },
  "200% text must expand the collapsed host around its title and button",
);
assert.deepEqual(
  { ...collapsedCardMetrics(160, 26) },
  { width: 160, height: 67 },
  "a narrow official card must cap collapsed width without clipping vertically",
);
const displayNoneSelectors = [...card.matchAll(/([^{}]+)\{([^{}]*display:\s*none[^{}]*)\}/gu)]
  .flatMap(([, selectors]) => selectors.split(",").map((selector) => selector.trim()));
assert.equal(
  displayNoneSelectors.includes(':host([data-layout="compact"]) .app-launcher'),
  false,
  "compact layout should keep the global app launcher available",
);
assert.equal(
  displayNoneSelectors.includes(':host([data-layout="collapsed"]) .app-launcher'),
  true,
  "collapsed layout should prioritize the expand control",
);
assert.equal(
  displayNoneSelectors.includes(':host([data-layout="compact"]) .recent-heading'),
  false,
  "compact layout must keep the recent-record heading visible",
);
assert.equal(
  displayNoneSelectors.includes(':host([data-layout="compact"]) .recent'),
  false,
  "compact layout must keep recent records visible",
);

console.log("codex card layout contract: ok");
