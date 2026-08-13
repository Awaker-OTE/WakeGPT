import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { Buffer } from "node:buffer";
import ts from "typescript";

const sourceUrl = new URL("../src/image-input.ts", import.meta.url);
const source = await readFile(sourceUrl, "utf8");
const output = ts.transpileModule(source, {
  compilerOptions: {
    module: ts.ModuleKind.ES2022,
    target: ts.ScriptTarget.ES2022,
  },
}).outputText;
const imageInput = await import(`data:text/javascript;base64,${Buffer.from(output).toString("base64")}`);

const png = { name: "截图.png", size: 1024, type: "image/png" };
assert.equal(imageInput.validateImageFiles([png], []), null);
assert.equal(
  imageInput.validateImageFiles([{ name: "截图.PNG", size: 1024, type: "" }], []),
  null,
);
assert.equal(
  imageInput.validateImageFiles([{ name: "vector.svg", size: 1024, type: "image/svg+xml" }], []),
  "仅支持 PNG、JPEG、WebP 和 GIF 图片。",
);
assert.equal(
  imageInput.validateImageFiles([{ ...png, size: 20 * 1024 * 1024 + 1 }], []),
  "单张图片不能超过 20 MiB。",
);
assert.equal(
  imageInput.validateImageFiles(Array.from({ length: 10 }, () => png), [{ byteSize: 1 }]),
  "每条记录最多 10 张图片。",
);
assert.equal(
  imageInput.validateImageFiles(
    [{ ...png, size: 20 * 1024 * 1024 }],
    Array.from({ length: 5 }, () => ({ byteSize: 17 * 1024 * 1024 })),
  ),
  "单条记录的图片合计不能超过 100 MiB。",
);
assert.equal(imageInput.safeImageDisplayName("folder/截图", "image/png"), "folder_截图.png");
assert.equal(imageInput.safeImageDisplayName("photo.jpeg", "image/jpeg"), "photo.jpeg");
assert.ok(new TextEncoder().encode(imageInput.safeImageDisplayName("图".repeat(300), "image/png")).byteLength <= 480);

console.log("image input validation: ok");
