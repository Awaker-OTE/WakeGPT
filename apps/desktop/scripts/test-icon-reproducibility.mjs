import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, readdir, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";

const appRoot = fileURLToPath(new URL("..", import.meta.url));
const temporaryRoot = await mkdtemp(join(tmpdir(), "wakegpt-icon-reproducibility-"));
const firstGeneratedDirectory = join(temporaryRoot, "first");
const secondGeneratedDirectory = join(temporaryRoot, "second");

const run = (command, arguments_) => {
  const result = spawnSync(command, arguments_, {
    cwd: appRoot,
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
  });
  assert.equal(result.status, 0, result.stderr || result.stdout);
};

const assertSameFile = async (first, second, label) => {
  assert.equal(
    Buffer.compare(await readFile(first), await readFile(second)),
    0,
    `${label} must be byte-identical`,
  );
};

const pngDimensions = async (path) => {
  const bytes = await readFile(path);
  assert.equal(bytes.subarray(1, 4).toString("ascii"), "PNG");
  return [bytes.readUInt32BE(16), bytes.readUInt32BE(20)];
};

const icnsChunkTypes = async (path) => {
  const bytes = await readFile(path);
  assert.equal(bytes.subarray(0, 4).toString("ascii"), "icns");
  assert.equal(bytes.readUInt32BE(4), bytes.length, "ICNS declared size must match the file");
  const types = [];
  for (let offset = 8; offset < bytes.length;) {
    const size = bytes.readUInt32BE(offset + 4);
    assert.ok(size >= 8 && offset + size <= bytes.length, "ICNS chunk bounds must be valid");
    types.push(bytes.subarray(offset, offset + 4).toString("ascii"));
    offset += size;
  }
  return types;
};

const relativeFiles = async (directory, prefix = "") => {
  const files = [];
  for (const entry of await readdir(join(directory, prefix), { withFileTypes: true })) {
    const relativePath = join(prefix, entry.name);
    if (entry.isDirectory()) files.push(...await relativeFiles(directory, relativePath));
    else if (entry.isFile()) files.push(relativePath);
  }
  return files.sort();
};

try {
  for (const outputDirectory of [firstGeneratedDirectory, secondGeneratedDirectory]) {
    run(process.execPath, ["scripts/generate-icons.mjs", "--output", outputDirectory]);
  }

  const pngFiles = (await readdir(firstGeneratedDirectory))
    .filter((name) => name.endsWith(".png"))
    .sort();
  assert.deepEqual(
    (await readdir(secondGeneratedDirectory)).filter((name) => name.endsWith(".png")).sort(),
    pngFiles,
    "both generations must produce the same top-level PNG set",
  );
  assert.equal(pngFiles.length, 15, "the full desktop and Windows PNG set must stay complete");
  for (const file of pngFiles) {
    await assertSameFile(
      join(firstGeneratedDirectory, file),
      join(secondGeneratedDirectory, file),
      file,
    );
  }
  await assertSameFile(
    join(firstGeneratedDirectory, "icon.ico"),
    join(secondGeneratedDirectory, "icon.ico"),
    "icon.ico",
  );
  await assertSameFile(
    join(firstGeneratedDirectory, "icon.icns"),
    join(secondGeneratedDirectory, "icon.icns"),
    "icon.icns",
  );
  for (const outputDirectory of [firstGeneratedDirectory, secondGeneratedDirectory]) {
    const types = await icnsChunkTypes(join(outputDirectory, "icon.icns"));
    assert.deepEqual(types, [...types].sort(), "ICNS chunks must use canonical type order");
  }

  for (const [file, expected] of [
    ["icon.png", [512, 512]],
    [`128x128${"@2x.png"}`, [256, 256]],
    ["128x128.png", [128, 128]],
    ["64x64.png", [64, 64]],
    ["32x32.png", [32, 32]],
  ]) {
    assert.deepEqual(await pngDimensions(join(firstGeneratedDirectory, file)), expected, file);
  }

  const [tauriConfig, packageManifest, rootIgnore] = await Promise.all([
    readFile(join(appRoot, "src-tauri", "tauri.conf.json"), "utf8").then(JSON.parse),
    readFile(join(appRoot, "package.json"), "utf8").then(JSON.parse),
    readFile(join(appRoot, "..", "..", ".gitignore"), "utf8"),
  ]);
  assert.match(tauriConfig.build.beforeBuildCommand, /npm run generate:icons/u);
  assert.equal(packageManifest.scripts["generate:icons"], "node scripts/generate-icons.mjs");
  for (const configuredPath of tauriConfig.bundle.icon) {
    assert.ok(
      (await relativeFiles(firstGeneratedDirectory)).includes(configuredPath.replace(/^icons\//u, "")),
      `${configuredPath} must be generated before bundling`,
    );
  }
  assert.match(rootIgnore, /apps\/desktop\/src-tauri\/icons\/\*/u);
  assert.match(rootIgnore, /!apps\/desktop\/src-tauri\/icons\/wakegpt-icon\.svg/u);

  if (process.platform === "darwin") {
    const firstIconset = join(temporaryRoot, "first.iconset");
    const secondIconset = join(temporaryRoot, "second.iconset");
    run("iconutil", ["-c", "iconset", "-o", firstIconset, join(firstGeneratedDirectory, "icon.icns")]);
    run("iconutil", ["-c", "iconset", "-o", secondIconset, join(secondGeneratedDirectory, "icon.icns")]);
    const firstLayers = await relativeFiles(firstIconset);
    assert.deepEqual(await relativeFiles(secondIconset), firstLayers, "ICNS layer sets must match");
    for (const layer of firstLayers) {
      await assertSameFile(join(firstIconset, layer), join(secondIconset, layer), `ICNS ${layer}`);
    }
  }
} finally {
  await rm(temporaryRoot, { recursive: true, force: true });
}

console.log("icon reproducibility contract: ok");
