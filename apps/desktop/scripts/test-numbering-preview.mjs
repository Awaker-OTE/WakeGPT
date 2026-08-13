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
  const { MarkdownPreview } = await server.ssrLoadModule("/src/ui.tsx");
  const html = renderToStaticMarkup(
    React.createElement(MarkdownPreview, {
      markdown: "41. First\n\n42. Second\n\n77. Explicit jump",
    }),
  );

  assert.match(html, /<ol start="41">/);
  assert.match(html, /<li value="41">[\s\S]*?<p>First<\/p>/);
  assert.match(html, /<li value="42">[\s\S]*?<p>Second<\/p>/);
  assert.match(html, /<li value="77">[\s\S]*?<p>Explicit jump<\/p>/);
  console.log("numbering preview rendering: ok");
} finally {
  await server.close();
}
