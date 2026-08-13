import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const [master, ui, app, css, card, adapter, lifecycle] =
  await Promise.all([
    readFile(new URL("../src-tauri/icons/wakegpt-icon.svg", import.meta.url), "utf8"),
    readFile(new URL("../src/ui.tsx", import.meta.url), "utf8"),
    readFile(new URL("../src/App.tsx", import.meta.url), "utf8"),
    readFile(new URL("../src/App.css", import.meta.url), "utf8"),
    readFile(new URL("../src-tauri/assets/codex_card_v1.js", import.meta.url), "utf8"),
    readFile(new URL("../src-tauri/src/codex_adapter.rs", import.meta.url), "utf8"),
    readFile(new URL("../src-tauri/src/lifecycle.rs", import.meta.url), "utf8"),
  ]);

assert.match(master, /WakeGPT double-note app icon/u);
assert.match(master, /#25bfd2/u);
assert.match(master, /#ffc23a/u);
assert.match(master, /rotate\(7 284 251\)/u);
assert.doesNotMatch(master, /<circle\b/u);

assert.match(ui, /export function WakeMark\(\)/u);
assert.match(ui, /<WakeMark \/>/u);
assert.match(app, /WakeMark,/u);
assert.equal((app.match(/<WakeMark \/>/gu) ?? []).length, 2);
assert.doesNotMatch(`${ui}\n${app}`, /className="brand-mark"[^>]*>W</u);
assert.match(css, /\.brand-mark \{[\s\S]*linear-gradient\(45deg, #25bfd2 0%, #b9d8bd 48%, #ffc23a 100%\)/u);

assert.match(card, /const wakeBrandMark = \(\) =>/u);
assert.match(card, /brandMark\.append\(wakeBrandMark\(\)\)/u);
assert.doesNotMatch(card, /element\("span", "mark", "W"\)/u);
assert.match(card, /codex-26\.803\.81509-v41/u);
assert.match(adapter, /codex-26\.803\.81509-v41/u);
assert.match(lifecycle, /fn menu_bar_template_icon\(\) -> Image<'static>/u);
assert.match(lifecycle, /template_icon_is_an_antialiased_w_monogram/u);
assert.match(lifecycle, /for x in 0\.\.22[\s\S]*alpha_at\(x, 0\)[\s\S]*alpha_at\(x, 17\)/u);
assert.match(lifecycle, /for y in 0\.\.18[\s\S]*alpha_at\(0, y\)[\s\S]*alpha_at\(21, y\)/u);
assert.match(lifecycle, /\.icon_as_template\(true\)/u);

console.log("WakeGPT brand mark contract: ok");
