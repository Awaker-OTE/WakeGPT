import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer } from "vite";

const [ui, bridge, commands, attachments, library, preview] = await Promise.all([
  readFile(new URL("../src/ui.tsx", import.meta.url), "utf8"),
  readFile(new URL("../src/bridge.ts", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/commands.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/attachments.rs", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8"),
  readFile(new URL("../src/preview.ts", import.meta.url), "utf8"),
]);

assert.match(bridge, /invoke<ArrayBuffer>\("read_markdown_image_preview"/u);
assert.match(library, /commands::read_markdown_image_preview/u);
assert.equal(
  (commands.match(/markdown_image_context\(&store, &request\)\?/gu) ?? []).length,
  2,
  "the active workspace and exact notebook identity must be checked before and after reading",
);
assert.match(commands, /notebook\.target_id != request\.notebook_target_id/u);
assert.match(commands, /response\.extend_from_slice\(b"WGMI"\)/u);
assert.match(commands, /response\.push\(1\)/u);
assert.match(attachments, /open_regular_file_read\(&relative\)/u);
assert.match(attachments, /markdown_image_symlink_rejected/u);
assert.match(attachments, /attachment_file_changed_during_read/u);

assert.match(ui, /new IntersectionObserver/u);
assert.match(ui, /readMarkdownImagePreview/u);
assert.match(ui, /URL\.revokeObjectURL/u);
assert.match(ui, /className="markdown-image-preview"/u);
assert.match(ui, /onOpen\(\{ url, label \}, event\.currentTarget\)/u);
assert.match(preview, /case "read_markdown_image_preview"/u);

const server = await createServer({
  logLevel: "silent",
  server: { middlewareMode: true },
  appType: "custom",
});

try {
  const { MarkdownPreview } = await server.ssrLoadModule("/src/ui.tsx");
  const html = renderToStaticMarkup(
    React.createElement(MarkdownPreview, {
      markdown: "![工作区图片](assets/reference.png)",
      workspaceId: "018f1111-1111-7111-8111-111111111111",
      notebookId: "018f2222-2222-7222-8222-222222222222",
      notebookTargetId: "018f9999-9999-7999-8999-999999999999",
    }),
  );
  assert.match(html, /class="markdown-image-preview"/u);
  assert.match(html, /data-state="idle"/u);
  assert.match(html, /正在读取本地图片 · 工作区图片/u);
  assert.doesNotMatch(html, /<img|assets\/reference\.png|wakegpt-markdown-image:/u);
} finally {
  await server.close();
}

console.log("markdown relative image preview contract: ok");
