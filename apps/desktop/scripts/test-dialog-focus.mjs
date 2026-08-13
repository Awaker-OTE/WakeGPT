import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import {
  dialogFocusTargetIndex,
  expandedControlInsideDialog,
} from "../src/dialogFocus.ts";

assert.equal(dialogFocusTargetIndex(-1, 3, false), 0, "Tab entering a dialog starts at the first item");
assert.equal(dialogFocusTargetIndex(-1, 3, true), 2, "Shift+Tab entering a dialog starts at the last item");
assert.equal(dialogFocusTargetIndex(2, 3, false), 0, "Tab wraps from the last item to the first");
assert.equal(dialogFocusTargetIndex(0, 3, true), 2, "Shift+Tab wraps from the first item to the last");
assert.equal(dialogFocusTargetIndex(1, 3, false), null, "interior forward focus uses native tab order");
assert.equal(dialogFocusTargetIndex(1, 3, true), null, "interior backward focus uses native tab order");
assert.equal(dialogFocusTargetIndex(-1, 0, false), null, "an empty dialog has no indexed target");

const originalDocument = globalThis.document;
const originalHTMLElement = globalThis.HTMLElement;
class FakeHTMLElement {
  constructor(expanded = false) { this.expanded = expanded; }
  closest(selector) { return selector === "[aria-expanded='true']" && this.expanded ? this : null; }
}
globalThis.HTMLElement = FakeHTMLElement;
const expanded = new FakeHTMLElement(true);
const dialog = { contains: (element) => element === expanded };
globalThis.document = { activeElement: expanded };
assert.equal(
  expandedControlInsideDialog(dialog),
  expanded,
  "an expanded composite inside the dialog owns Escape before the modal",
);
globalThis.document = { activeElement: new FakeHTMLElement() };
assert.equal(
  expandedControlInsideDialog(dialog),
  null,
  "a composite outside the dialog cannot suppress modal Escape",
);
globalThis.document = originalDocument;
globalThis.HTMLElement = originalHTMLElement;

const ui = await readFile(new URL("../src/ui.tsx", import.meta.url), "utf8");
assert.match(ui, /topmostModalDialog\(\) !== dialog/u, "only the topmost modal may trap focus");
assert.match(ui, /dialogFocusTargetIndex\(currentIndex, focusable\.length, event\.shiftKey\)/u);
assert.match(ui, /window\.addEventListener\("keydown", handleKeyDown, true\)/u);
assert.match(
  ui,
  /const returnFocusRef = useRef<HTMLElement \| null>\([\s\S]*?document\.activeElement[\s\S]*?\);[\s\S]*?useEffect\(\(\) => \{[\s\S]*?const previouslyFocused = returnFocusRef\.current;/u,
  "the invoker must be captured before descendant autoFocus runs",
);
assert.match(ui, /previouslyFocused\?\.isConnected/u, "focus restoration must reject removed invokers");
assert.match(ui, /remainingDialog && !remainingDialog\.contains\(previouslyFocused\)/u);
assert.match(ui, /aria-modal="true"[^>]*tabIndex=\{-1\}/u, "a dialog without controls needs a focus fallback");
assert.match(
  ui,
  /event\.key === "Escape"[\s\S]*expandedControlInsideDialog\(dialog\)[\s\S]*if \(closeDisabledRef\.current\) return;[\s\S]*closeRef\.current\(\)/u,
  "an expanded composite must close first and a busy modal must refuse Escape dismissal",
);
assert.match(
  ui,
  /const handleBackdrop = \(event:[\s\S]*if \(!closeDisabled && event\.target === event\.currentTarget\) onClose\(\)/u,
  "a busy modal must refuse backdrop dismissal",
);
assert.match(
  ui,
  /const closeDisabledRef = useRef\(closeDisabled\);[\s\S]*closeDisabledRef\.current = closeDisabled;[\s\S]*\}, \[\]\);/u,
  "busy-state changes must not reinstall the modal listener or restore focus early",
);

console.log("dialog focus lifecycle: ok");
