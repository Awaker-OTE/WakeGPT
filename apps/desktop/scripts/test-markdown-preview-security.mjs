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
  const { MarkdownPreview, classifyMarkdownImageSource } = await server.ssrLoadModule("/src/ui.tsx");
  const html = renderToStaticMarkup(
    React.createElement(MarkdownPreview, {
      markdown: `# 安全预览

**加粗**、*强调*、~~删除~~和 \`行内代码\`。

[安全链接](https://example.com/docs) [危险链接](javascript:alert(1)) [相对链接](../private)

![远程图片](https://tracking.example/pixel.png)
![本地图片](assets/reference%20image.png)
![越界图片](../private.png)

| 项目 | 状态 |
| --- | --- |
| 表格 | 正常 |

- [x] 已完成
- [ ] 未完成

脚注引用[^1]

[^1]: 脚注正文

<script>window.__wakegptPreviewPwned = true</script>
<img src=x onerror="window.__wakegptPreviewPwned = true">`,
    }),
  );

  assert.match(html, /<strong>加粗<\/strong>/u);
  assert.match(html, /<em>强调<\/em>/u);
  assert.match(html, /<del>删除<\/del>/u);
  assert.match(html, /<code>行内代码<\/code>/u);
  assert.match(html, /<table>/u);
  assert.match(html, /type="checkbox"[^>]*disabled=""[^>]*checked=""/u);
  assert.match(html, /远程图片已阻止 · 远程图片/u);
  assert.match(html, /本地图片仅在当前笔记中加载 · 本地图片/u);
  assert.match(html, /不安全图片路径已阻止 · 越界图片/u);
  assert.match(html, /href="https:\/\/example\.com\/docs"[^>]*target="_blank"[^>]*rel="noopener noreferrer"/u);
  assert.match(html, /class="markdown-link-disabled">危险链接<\/span>/u);
  assert.match(html, /class="markdown-link-disabled">相对链接<\/span>/u);
  assert.match(html, /data-footnotes="true"/u);
  assert.doesNotMatch(html, /<script|<img|javascript:|tracking\.example|onerror|__wakegptPreviewPwned|wakegpt-markdown-image:/u);

  assert.deepEqual(classifyMarkdownImageSource("assets/reference%20image.png"), {
    kind: "relative",
    relativePath: "assets/reference image.png",
  });
  for (const source of [
    "../outside.png",
    "/tmp/outside.png",
    "file:///tmp/outside.png",
    "javascript:alert(1)",
    "%2e%2e%2foutside.png",
    "assets\\outside.png",
  ]) {
    assert.notEqual(classifyMarkdownImageSource(source).kind, "relative", source);
  }
  assert.equal(classifyMarkdownImageSource("https://tracking.example/pixel.png").kind, "remote");

  console.log("markdown preview security and GFM: ok");
} finally {
  await server.close();
}
