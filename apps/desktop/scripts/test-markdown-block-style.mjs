import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createServer } from "vite";

const server = await createServer({
  logLevel: "silent",
  server: { middlewareMode: true },
  appType: "custom",
});

try {
  const { detectMarkdownBlockStyle, stripMarkdownBlockMarkers } = await server.ssrLoadModule("/src/ui.tsx");
  const ui = await readFile(new URL("../src/ui.tsx", import.meta.url), "utf8");

  assert.equal(detectMarkdownBlockStyle("普通正文", 2, 2), "paragraph");
  assert.equal(detectMarkdownBlockStyle("# 一级标题", 3, 3), "heading1");
  assert.equal(detectMarkdownBlockStyle("## 二级标题", 4, 4), "heading2");
  assert.equal(detectMarkdownBlockStyle("> 第一行\n> 第二行", 0, 11), "quote");
  assert.equal(detectMarkdownBlockStyle(">无空格引用", 3, 3), "quote");
  assert.equal(detectMarkdownBlockStyle("```ts\nconst answer = 42;\n```", 10, 10), "code");
  assert.equal(detectMarkdownBlockStyle("```\ncode\n```", 0, 12), "code");
  assert.equal(detectMarkdownBlockStyle("正文\n# 标题", 0, 7), "paragraph");
  assert.equal(detectMarkdownBlockStyle("```\ncode\n```\n正文", 15, 15), "paragraph");
  assert.equal(stripMarkdownBlockMarkers(">无空格引用"), "无空格引用");
  assert.equal(stripMarkdownBlockMarkers("~~~ts\nconst value = 1;\n~~~"), "const value = 1;");
  assert.match(ui, /onClick=\{\(event\) => rememberSelection\(event\.currentTarget\)\}/u);
  assert.match(ui, /onKeyUp=\{\(event\) => rememberSelection\(event\.currentTarget\)\}/u);

  console.log("markdown block style detection: ok");
} finally {
  await server.close();
}
