import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createServer } from "vite";

const [uiSource, appCss] = await Promise.all([
  readFile(new URL("../src/ui.tsx", import.meta.url), "utf8"),
  readFile(new URL("../src/App.css", import.meta.url), "utf8"),
]);
const server = await createServer({
  logLevel: "silent",
  server: { middlewareMode: true },
  appType: "custom",
});

try {
  const { anchoredOverlayPosition } = await server.ssrLoadModule("/src/ui.tsx");
  const rect = (left, top, width, height) => ({
    left,
    right: left + width,
    top,
    bottom: top + height,
    width,
    height,
  });

  assert.deepEqual(
    anchoredOverlayPosition(rect(20, 20, 100, 34), { width: 180, height: 120 }, { width: 800, height: 600 }, { width: 180 }),
    { left: 20, top: 60, width: 180, placement: "below" },
  );
  assert.deepEqual(
    anchoredOverlayPosition(rect(760, 40, 32, 32), { width: 180, height: 100 }, { width: 800, height: 600 }, { width: 180, align: "end" }),
    { left: 612, top: 78, width: 180, placement: "below" },
  );
  assert.deepEqual(
    anchoredOverlayPosition(rect(250, 550, 80, 30), { width: 190, height: 120 }, { width: 800, height: 600 }, { width: 190 }),
    { left: 250, top: 424, width: 190, placement: "above" },
  );
  assert.deepEqual(
    anchoredOverlayPosition(rect(2, 2, 30, 30), { width: 240, height: 100 }, { width: 160, height: 300 }, { width: 240 }),
    { left: 8, top: 38, width: 144, placement: "below" },
  );
  assert.deepEqual(
    anchoredOverlayPosition(rect(70, 280, 20, 20), { width: 120, height: 584 }, { width: 400, height: 600 }),
    { left: 70, top: 8, width: 120, placement: "below" },
  );

  assert.match(uiSource, /setPosition\(anchoredOverlayPosition\(/u);
  assert.match(uiSource, /setWorkspaceMenuPosition\(anchoredOverlayPosition\(/u);
  assert.match(
    uiSource,
    /active && workspaceMenuOpen \? createPortal\([\s\S]*?className="workspace-menu"[\s\S]*?document\.body/u,
    "the workspace menu must escape the scrolling sidebar through a document-level portal",
  );
  assert.match(uiSource, /window\.addEventListener\("scroll", handleViewportChange, true\)/u);
  assert.match(
    uiSource,
    /event\.type === "scroll" && target instanceof Node[\s\S]*?workspaceMenuElementRef\.current\?\.contains\(target\)\) return/u,
    "scrolling a tall workspace menu must not close the menu itself",
  );
  assert.match(
    appCss,
    /\.workspace-menu \{[^}]*position:\s*fixed;[^}]*max-height:\s*calc\(100vh - 16px\);[^}]*overflow:\s*auto;/u,
  );
  assert.match(appCss, /@media \(forced-colors: active\)[\s\S]*\.workspace-menu/u);
} finally {
  await server.close();
}

console.log("anchored overlay positioning contract: ok");
