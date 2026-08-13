import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const [appCss, card] = await Promise.all([
  readFile(new URL("../src/App.css", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/assets/codex_card_v1.js", import.meta.url), "utf8"),
]);

for (const [name, source, prefix, tokens] of [
  ["desktop", appCss, "type", [
    ["title", "1.0625rem"], ["body", "0.875rem"], ["control", "0.8125rem"],
    ["label", "0.75rem"], ["meta", "0.6875rem"], ["caption", "0.625rem"],
  ]],
  ["Codex card", card, "wake-type", [
    ["title", ".875rem"], ["body", ".8125rem"], ["control", ".8125rem"],
    ["label", ".75rem"], ["meta", ".6875rem"], ["caption", ".625rem"],
  ]],
]) {
  for (const [token, value] of tokens) {
    assert.match(
      source,
      new RegExp(`--${prefix}-${token}:\\s*${value.replace(".", "\\.")}`, "u"),
      `${name} must declare the shared ${token} typography token`,
    );
  }
}

assert.match(
  appCss,
  /:root \{[^}]*font-size:\s*100%;[^}]*line-height:\s*var\(--leading-body\);/u,
  "desktop root must preserve the user agent text-size baseline",
);
assert.match(
  appCss,
  /body \{[^}]*font-size:\s*var\(--type-body\);[^}]*line-height:\s*var\(--leading-body\);/u,
  "desktop body must consume the relative body scale",
);
assert.match(appCss, /\.sidebar-brand strong \{[^}]*font-size:\s*var\(--type-title\);/u);
assert.match(appCss, /\.select-option \{[^}]*font-size:\s*var\(--type-label\);/u);
assert.match(appCss, /\.attachment-tile__meta small \{[^}]*font-size:\s*var\(--type-caption\);/u);

assert.match(
  card,
  /:host \{[^}]*font-family:[^;}]+;[^}]*font-size:var\(--wake-type-control\);[^}]*line-height:var\(--wake-leading-body\);/u,
  "all Shadow DOM siblings must inherit the same card typography root",
);
assert.match(card, /\.card \{[^}]*font:inherit;/u);
assert.match(card, /header strong \{[^}]*font-size:var\(--wake-type-title\);/u);
assert.match(card, /\.wake-select-option \{[^}]*font-size:var\(--wake-type-control\);/u);
assert.match(card, /\.recent-heading \{[^}]*font-size:var\(--wake-type-label\);/u);
assert.match(card, /\.item-body \{[^}]*font-size:var\(--wake-type-body\);/u);
assert.match(card, /\.item-meta \{[^}]*font-size:var\(--wake-type-meta\);/u);
assert.match(card, /\.item-menu \{[^}]*font-size:var\(--wake-type-body\);/u);

const fixedPixelTypography = /(?:(?:--(?:wake-)?type-[\w-]+|font-size)|font)\s*:[^;{}]*\d+(?:\.\d+)?px/gu;
assert.deepEqual(
  [...appCss.matchAll(fixedPixelTypography), ...card.matchAll(fixedPixelTypography)].map((match) => match[0]),
  [],
  "readable UI text must scale from the user agent baseline instead of fixed pixel sizes",
);
assert.match(appCss, /\.select-trigger \{[^}]*min-height:\s*34px;[^}]*padding:\s*6px/u);
assert.match(card, /\.wake-select-trigger \{[^}]*min-height:28px;[^}]*padding:3px/u);
assert.match(
  appCss,
  /\.notes-view, \.utility-view, \.settings-view \{[^}]*width:\s*100%;[^}]*flex:\s*1;/u,
  "all flex-column content views must fill the available width before their max-width cap",
);
assert.match(
  appCss,
  /\.app-sidebar \{[^}]*overflow-x:\s*hidden;[^}]*overflow-y:\s*auto;[^}]*scrollbar-gutter:\s*stable;/u,
  "the whole sidebar must remain vertically reachable with large text or a short viewport",
);
assert.match(
  appCss,
  /\.app-shell \{[^}]*grid-template-columns:\s*clamp\(268px,\s*16\.75rem,\s*34vw\) minmax\(0, 1fr\);/u,
  "the desktop sidebar must grow with the root text size without taking over the whole window",
);
assert.match(
  appCss,
  /\.sidebar-brand \{[^}]*grid-template-columns:\s*30px minmax\(0, 1fr\) 30px;/u,
  "large brand text must not push the quick-capture control outside the sidebar",
);
assert.match(
  appCss,
  /\.sidebar-brand strong \{[^}]*min-width:\s*0;[^}]*overflow-wrap:\s*anywhere;/u,
  "the brand label must reflow instead of obscuring adjacent controls",
);
assert.match(
  appCss,
  /\.notebook-tab \{[^}]*min-width:\s*max\(96px,\s*6em\);[^}]*max-width:\s*max\(188px,\s*12em\);[^}]*flex:\s*0 0 auto;/u,
  "large notebook labels must keep their own width and use the tab strip for horizontal overflow",
);
assert.doesNotMatch(
  appCss,
  /@media \(max-width:\s*65\.625rem\)[\s\S]*?\.app-shell \{\s*grid-template-columns:\s*228px/u,
  "a narrow viewport must not erase the text-relative sidebar width",
);
assert.match(
  appCss,
  /\.quick-capture-scroll \{[^}]*overflow-x:\s*hidden;[^}]*overflow-y:\s*auto;[^}]*scrollbar-gutter:\s*stable;/u,
  "large quick-capture content must remain vertically reachable without page-level overflow",
);
assert.match(
  card,
  /\.card-scroll \{[^}]*min-height:0;[^}]*overflow-y:auto;[^}]*scrollbar-gutter:stable;/u,
  "large card content must remain reachable inside its bounded host",
);
assert.match(
  appCss,
  /\.view-heading__identity \{[^}]*min-width:\s*0;[^}]*flex:\s*1 1 240px;/u,
  "long workspace and notebook names must not grow the page header past its content column",
);
assert.match(
  appCss,
  /\.view-heading h1 \{[^}]*display:\s*-webkit-box;[^}]*max-height:\s*2\.5em;[^}]*overflow:\s*hidden;[^}]*overflow-wrap:\s*anywhere;[^}]*-webkit-line-clamp:\s*2;/u,
  "long notebook titles must use a bounded two-line visual label",
);
assert.match(
  appCss,
  /\.target-status__text \{[^}]*display:\s*-webkit-box;[^}]*max-height:\s*2\.7em;[^}]*overflow:\s*hidden;[^}]*overflow-wrap:\s*anywhere;[^}]*-webkit-line-clamp:\s*2;/u,
  "long sync paths must use a bounded two-line visual status",
);
assert.match(
  appCss,
  /@media \(max-width:\s*65\.625rem\)[\s\S]*?\.view-heading__actions \{[^}]*flex:\s*0 1 min\(50%, 360px\);[^}]*justify-content:\s*flex-end;/u,
  "large text at the minimum window must keep page identity and actions on one bounded header row",
);
assert.match(
  appCss,
  /\.record-actions \{[^}]*position:\s*absolute;[^}]*top:\s*12px;[^}]*right:\s*12px;/u,
  "record actions must remain reachable at the top of arbitrarily long records",
);
assert.match(
  appCss,
  /\.record-list \{[^}]*min-width:\s*0;[^}]*grid-template-columns:\s*minmax\(0, 1fr\);/u,
  "unbroken record text must not enlarge the implicit grid track",
);
assert.match(
  appCss,
  /\.record-card \{[^}]*width:\s*100%;[^}]*min-width:\s*0;/u,
  "record cards must shrink inside the notes column",
);
assert.match(
  appCss,
  /\.dialog h2 \{[^}]*display:\s*-webkit-box;[^}]*max-height:\s*2\.7em;[^}]*overflow-wrap:\s*anywhere;[^}]*-webkit-line-clamp:\s*2;/u,
  "long dialog titles must remain bounded without pushing dialog content away",
);
assert.match(
  appCss,
  /\.markdown-preview-pagination \{[^}]*position:\s*sticky;[^}]*min-width:\s*0;[^}]*border-bottom:[^}]*font-size:/u,
  "large Markdown paging controls must remain visible inside the preview scroll owner",
);
assert.match(appCss, /\.markdown-preview-pagination p \{[^}]*overflow-wrap:\s*anywhere;/u);
assert.match(
  appCss,
  /\.markdown-preview-oversized-block \{[^}]*white-space:\s*pre-wrap;[^}]*overflow-wrap:\s*anywhere;/u,
  "an oversized Markdown block must wrap as safe plain text instead of widening the dialog",
);
assert.match(
  appCss,
  /\.conflict-inspector__meta > span \{[^}]*min-width:\s*0;[^}]*max-width:\s*100%;[^}]*overflow-wrap:\s*anywhere;/u,
  "long conflict paths must wrap inside the dialog instead of creating horizontal overflow",
);
assert.match(
  card,
  /\.target-hint \{[^}]*display:-webkit-box;[^}]*max-height:calc\(2\.7em \+ 15px\);[^}]*overflow-wrap:anywhere;[^}]*-webkit-line-clamp:2;/u,
  "the card target hint must bound long notebook names while retaining a reachable composer",
);

console.log("typography contract: ok");
