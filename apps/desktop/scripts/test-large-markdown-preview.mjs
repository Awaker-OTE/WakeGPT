import assert from "node:assert/strict";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer } from "vite";

const server = await createServer({
  logLevel: "silent",
  server: { middlewareMode: true },
  appType: "custom",
});

try {
  const [{ MarkdownPreview, restoreMarkdownLineEndings }, pages] = await Promise.all([
    server.ssrLoadModule("/src/ui.tsx"),
    server.ssrLoadModule("/src/markdown-preview-pages.ts"),
  ]);
  const {
    MARKDOWN_LIVE_PREVIEW_MAX_BYTES,
    MARKDOWN_DOCUMENT_MAX_BYTES,
    MARKDOWN_PREVIEW_PAGE_MAX_BYTES,
    markdownPreviewRequiresPaging,
    paginateMarkdownEditing,
    paginateMarkdownPreview,
  } = pages;

  const block = "## 分页标题\n\n这是包含 **粗体**、[安全链接](https://example.com) 和 `code` 的段落。\n\n";
  const largeMarkdown = block.repeat(
    Math.ceil((MARKDOWN_LIVE_PREVIEW_MAX_BYTES * 2) / Buffer.byteLength(block)),
  );
  assert.equal(markdownPreviewRequiresPaging(largeMarkdown), true);

  const start = performance.now();
  const result = paginateMarkdownPreview(largeMarkdown);
  const paginationMs = performance.now() - start;
  assert.ok(result.length >= 3, "large Markdown must be split into multiple bounded pages");
  assert.equal(result.map((page) => page.markdown).join(""), largeMarkdown);
  assert.ok(result.every((page) => page.start <= page.end));
  assert.ok(result.every((page, index) => index === 0 || page.start === result[index - 1].end));
  assert.ok(result.every((page) => page.byteLength <= MARKDOWN_PREVIEW_PAGE_MAX_BYTES));
  assert.ok(paginationMs < 400, `pagination took ${Math.round(paginationMs)} ms`);

  const htmlStart = performance.now();
  const html = renderToStaticMarkup(React.createElement(MarkdownPreview, {
    markdown: largeMarkdown,
  }));
  const renderMs = performance.now() - htmlStart;
  assert.match(html, /大文档预览分页/u);
  assert.match(html, /第 1 \/ [3-9][0-9]* 页/u);
  assert.match(html, /上一页/u);
  assert.match(html, /下一页/u);
  assert.doesNotMatch(html, /第 2 \/ /u);
  assert.ok(renderMs < 2_500, `first large-document preview took ${Math.round(renderMs)} ms`);

  const hugeFence = `\`\`\`text\n${"x".repeat(MARKDOWN_LIVE_PREVIEW_MAX_BYTES + 64)}\n\`\`\`\n`;
  const hugeFencePages = paginateMarkdownPreview(hugeFence);
  assert.ok(hugeFencePages.length > 1);
  assert.equal(hugeFencePages.map((page) => page.markdown).join(""), hugeFence);
  assert.ok(hugeFencePages.every((page) => page.oversizedBlock));
  assert.ok(hugeFencePages.every(
    (page) => page.byteLength <= MARKDOWN_PREVIEW_PAGE_MAX_BYTES,
  ));
  const hugeFenceHtml = renderToStaticMarkup(React.createElement(MarkdownPreview, {
    markdown: hugeFence,
  }));
  assert.match(hugeFenceHtml, /当前单个 Markdown 块过大/u);
  assert.match(hugeFenceHtml, /markdown-preview-oversized-block/u);
  assert.doesNotMatch(hugeFenceHtml, /<code/u);

  const maximumDocument = `${block.repeat(
    Math.ceil(MARKDOWN_DOCUMENT_MAX_BYTES / Buffer.byteLength(block)),
  )}`.slice(0, MARKDOWN_DOCUMENT_MAX_BYTES);
  const maximumStart = performance.now();
  const maximumPages = paginateMarkdownPreview(maximumDocument);
  const maximumMs = performance.now() - maximumStart;
  assert.equal(maximumPages.map((page) => page.markdown).join(""), maximumDocument);
  assert.ok(maximumPages.length > 20);
  assert.ok(maximumMs < 2_500, `16 MiB pagination took ${Math.round(maximumMs)} ms`);
  const maximumEditingPages = paginateMarkdownEditing(maximumDocument);
  assert.equal(maximumEditingPages.map((page) => page.markdown).join(""), maximumDocument);
  assert.ok(maximumEditingPages.every(
    (page) => Buffer.byteLength(page.markdown) <= MARKDOWN_PREVIEW_PAGE_MAX_BYTES,
  ));

  const maximumContinuousLine = "😀".repeat(MARKDOWN_DOCUMENT_MAX_BYTES / 4);
  const continuousPages = paginateMarkdownEditing(maximumContinuousLine);
  assert.equal(continuousPages.map((page) => page.markdown).join(""), maximumContinuousLine);
  assert.ok(continuousPages.length > 20);
  assert.ok(continuousPages.every((page) => page.plainTextPreview));
  assert.ok(continuousPages.every(
    (page) => Buffer.byteLength(page.markdown) <= MARKDOWN_PREVIEW_PAGE_MAX_BYTES,
  ));
  const continuousPreviewPages = paginateMarkdownPreview(maximumContinuousLine);
  assert.equal(
    continuousPreviewPages.map((page) => page.markdown).join(""),
    maximumContinuousLine,
  );
  assert.ok(continuousPreviewPages.every((page) => page.oversizedBlock));
  assert.ok(continuousPreviewPages.every(
    (page) => page.byteLength <= MARKDOWN_PREVIEW_PAGE_MAX_BYTES,
  ));

  assert.equal(restoreMarkdownLineEndings("a\nb\n", "\n"), "a\nb\n");
  assert.equal(restoreMarkdownLineEndings("a\nb\n", "\r\n"), "a\r\nb\r\n");
  assert.equal(restoreMarkdownLineEndings("a\r\nb\rc\n", "\n"), "a\nb\nc\n");
  assert.equal(restoreMarkdownLineEndings("a\r\nb\rc\n", "\r\n"), "a\r\nb\r\nc\r\n");
  assert.equal(restoreMarkdownLineEndings("a\nb\n", "\r"), "a\rb\r");
  assert.equal(restoreMarkdownLineEndings("a\r\nb\rc\n", "\r"), "a\rb\rc\r");

  console.log("large Markdown preview paging: ok");
} finally {
  await server.close();
}
