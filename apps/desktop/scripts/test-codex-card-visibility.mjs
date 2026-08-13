import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import vm from "node:vm";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const cardScript = readFileSync(
  join(scriptDirectory, "../src-tauri/assets/codex_card_v1.js"),
  "utf8",
);
const mountMarker = "globalThis.__wakegptMountCodexCard = mount;";
assert.equal(cardScript.includes(mountMarker), true, "card test hook marker is missing");
const instrumentedScript = cardScript.replace(
  mountMarker,
  `globalThis.__wakegptVisibilityTest = { pageBlockReason, pageBlocksCard };\n  ${mountMarker}`,
);

class FakeElement {
  constructor({ tag = "div", rect, style = {}, attributes = {}, parent = null }) {
    this.tagName = tag.toUpperCase();
    this.rect = rect;
    this.style = {
      display: "block",
      visibility: "visible",
      opacity: "1",
      position: "static",
      pointerEvents: "auto",
      ...style,
    };
    this.attributes = new Map(Object.entries(attributes));
    this.parentElement = parent;
    this.children = [];
    if (parent) parent.children.push(this);
  }

  getBoundingClientRect() {
    return this.rect;
  }

  getAttribute(name) {
    return this.attributes.get(name) ?? null;
  }

  contains(candidate) {
    for (let node = candidate; node; node = node.parentElement) {
      if (node === this) return true;
    }
    return false;
  }

  closest(selector) {
    if (selector !== "#wakegpt-codex-card-v1") return null;
    for (let node = this; node; node = node.parentElement) {
      if (node.getAttribute("id") === "wakegpt-codex-card-v1") return node;
    }
    return null;
  }
}

const viewport = { width: 1920, height: 1080 };
const fullViewport = { left: 0, top: 0, right: 1920, bottom: 1080, width: 1920, height: 1080 };
let elements = [];
let modalElement = null;

const document = {
  body: {},
  fullscreenElement: null,
  querySelector(selector) {
    if (selector === ":modal") return modalElement;
    return null;
  },
  querySelectorAll(selector) {
    if (selector === "body *") return elements;
    if (selector === "[aria-modal='true'],[role='dialog']") {
      return elements.filter((element) =>
        element.getAttribute("aria-modal") === "true"
        || element.getAttribute("role") === "dialog"
      );
    }
    if (selector === "img,video,canvas") {
      return elements.filter((element) => ["IMG", "VIDEO", "CANVAS"].includes(element.tagName));
    }
    return [];
  },
};

const context = vm.createContext({
  console,
  document,
  globalThis: null,
  HTMLElement: FakeElement,
  HTMLTextAreaElement: class extends FakeElement {},
  InputEvent: class {},
  MutationObserver: class {},
  ResizeObserver: class {},
  window: { innerWidth: viewport.width, innerHeight: viewport.height },
  getComputedStyle: (element) => element.style,
});
context.globalThis = context;
vm.runInContext(instrumentedScript, context, { filename: "codex_card_v1.js" });
const pageBlocksCard = context.__wakegptVisibilityTest?.pageBlocksCard;
const pageBlockReason = context.__wakegptVisibilityTest?.pageBlockReason;
assert.equal(typeof pageBlocksCard, "function", "card must expose its internal visibility predicate to the fixture");
assert.equal(typeof pageBlockReason, "function", "card must expose its fixed visibility reason to the fixture");

const setElements = (...nextElements) => {
  elements = nextElements;
  modalElement = null;
  document.fullscreenElement = null;
};

const toastLayer = new FakeElement({
  rect: fullViewport,
  style: { position: "fixed", pointerEvents: "none" },
});
setElements(toastLayer);
assert.equal(pageBlockReason(), null, "an empty fixed toast layer has no blocking reason");
assert.equal(pageBlocksCard(), false, "an empty fixed toast layer must not hide WakeGPT");

const inlineImage = new FakeElement({
  tag: "img",
  rect: { left: 300, top: 180, right: 1200, bottom: 780, width: 900, height: 600 },
});
setElements(inlineImage);
assert.equal(pageBlockReason(), null, "a large inline conversation image has no blocking reason");
assert.equal(pageBlocksCard(), false, "a large inline conversation image must not hide WakeGPT");

const fullscreenSurface = new FakeElement({ rect: fullViewport });
setElements(fullscreenSurface);
document.fullscreenElement = fullscreenSurface;
assert.equal(pageBlockReason(), "fullscreen", "fullscreen must use the fixed fullscreen reason");
assert.equal(pageBlocksCard(), true, "fullscreen must hide WakeGPT");

const lightbox = new FakeElement({
  rect: fullViewport,
  style: { position: "fixed", pointerEvents: "none" },
});
const lightboxImage = new FakeElement({
  tag: "img",
  rect: { left: 420, top: 80, right: 1500, bottom: 980, width: 1080, height: 900 },
  style: { pointerEvents: "auto" },
  parent: lightbox,
});
setElements(lightbox, lightboxImage);
assert.equal(
  pageBlockReason(),
  "mediaLightbox",
  "a fixed media lightbox must use the fixed mediaLightbox reason",
);
assert.equal(pageBlocksCard(), true, "a fixed full-viewport image lightbox must hide WakeGPT");

const modalDialog = new FakeElement({
  rect: { left: 600, top: 220, right: 1320, bottom: 860, width: 720, height: 640 },
  style: { position: "fixed" },
  attributes: { role: "dialog", "aria-modal": "true" },
});
setElements(modalDialog);
assert.equal(pageBlockReason(), "modal", "an explicit modal must use the fixed modal reason");
assert.equal(pageBlocksCard(), true, "an explicit modal dialog must hide WakeGPT");

const nonModalDialog = new FakeElement({
  rect: { left: 20, top: 20, right: 320, bottom: 220, width: 300, height: 200 },
  style: { position: "absolute" },
  attributes: { role: "dialog" },
});
setElements(nonModalDialog);
assert.equal(pageBlockReason(), null, "a small non-modal popover has no blocking reason");
assert.equal(pageBlocksCard(), false, "a small non-modal popover must not hide WakeGPT");

const quickChatLayer = new FakeElement({
  rect: fullViewport,
  style: { position: "fixed", pointerEvents: "none" },
});
const quickChatSurface = new FakeElement({
  rect: { left: 80, top: 60, right: 1840, bottom: 1040, width: 1760, height: 980 },
  style: {
    position: "relative",
    pointerEvents: "auto",
    backgroundColor: "rgb(255, 255, 255)",
    borderTopLeftRadius: "36px",
    boxShadow: "0 18px 60px rgba(0, 0, 0, 0.18)",
  },
  parent: quickChatLayer,
});
setElements(quickChatLayer, quickChatSurface);
assert.equal(
  pageBlockReason(),
  "viewportOverlay",
  "a quick-chat surface must use the fixed viewportOverlay reason",
);
assert.equal(
  pageBlocksCard(),
  true,
  "a large fixed quick-chat surface without dialog semantics must hide WakeGPT",
);
setElements();
assert.equal(pageBlockReason(), null, "closing quick chat must clear the blocking reason");
assert.equal(pageBlocksCard(), false, "closing quick chat must restore WakeGPT");

const smallFloatingUtility = new FakeElement({
  rect: { left: 1200, top: 160, right: 1740, bottom: 760, width: 540, height: 600 },
  style: {
    position: "fixed",
    pointerEvents: "auto",
    backgroundColor: "rgb(255, 255, 255)",
    borderTopLeftRadius: "24px",
    boxShadow: "0 12px 40px rgba(0, 0, 0, 0.15)",
  },
});
setElements(smallFloatingUtility);
assert.equal(pageBlockReason(), null, "a bounded floating utility has no blocking reason");
assert.equal(pageBlocksCard(), false, "a bounded floating utility must not hide WakeGPT");

const hiddenLightbox = new FakeElement({
  rect: fullViewport,
  style: { position: "fixed", display: "none" },
});
const hiddenImage = new FakeElement({
  tag: "img",
  rect: { left: 420, top: 80, right: 1500, bottom: 980, width: 1080, height: 900 },
  parent: hiddenLightbox,
});
setElements(hiddenLightbox, hiddenImage);
assert.equal(pageBlockReason(), null, "a closed lightbox must clear the blocking reason");
assert.equal(pageBlocksCard(), false, "a closed lightbox must restore WakeGPT visibility");

console.log("codex card visibility fixture: ok");
