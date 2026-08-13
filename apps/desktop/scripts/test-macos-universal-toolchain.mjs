import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const appRoot = fileURLToPath(new URL("..", import.meta.url));
const builder = fileURLToPath(new URL("./build-macos-universal.mjs", import.meta.url));

if (process.platform !== "darwin") {
  console.log("macOS Universal rustup toolchain contract: skipped outside macOS");
  process.exit(0);
}

const result = spawnSync(process.execPath, [builder, "--check-only"], {
  cwd: appRoot,
  encoding: "utf8",
  maxBuffer: 16 * 1024 * 1024,
});
assert.equal(result.status, 0, result.stderr || result.stdout);
const report = JSON.parse(result.stdout.trim());
assert.equal(report.source, "rustup");
assert.equal(report.toolchain, "1.96.0");
assert.match(report.host, /-apple-darwin$/u);
assert.deepEqual(report.targets, ["aarch64-apple-darwin", "x86_64-apple-darwin"]);

const packageJson = JSON.parse(await readFile(join(appRoot, "package.json"), "utf8"));
assert.equal(
  packageJson.scripts["release:macos-universal"],
  "node scripts/build-macos-universal.mjs",
);

const unknown = spawnSync(process.execPath, [builder, "--target", "aarch64-apple-darwin"], {
  cwd: appRoot,
  encoding: "utf8",
});
assert.equal(unknown.status, 1, unknown.stderr || unknown.stdout);
assert.match(unknown.stderr, /Only --check-only is supported/u);

console.log("macOS Universal rustup toolchain contract: ok");
