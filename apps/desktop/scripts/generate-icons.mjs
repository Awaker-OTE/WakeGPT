import { randomUUID } from "node:crypto";
import { spawnSync } from "node:child_process";
import { lstat, readFile, rename, rm, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const appRoot = fileURLToPath(new URL("..", import.meta.url));
const npm = process.platform === "win32" ? "npm.cmd" : "npm";

const parseOutputDirectory = (arguments_) => {
  if (arguments_.length === 0) return join(appRoot, "src-tauri", "icons");
  if (arguments_.length !== 2 || arguments_[0] !== "--output" || !arguments_[1]) {
    throw new Error("Usage: generate-icons.mjs [--output <directory>]");
  }
  return resolve(arguments_[1]);
};

const canonicalizeIcns = async (path) => {
  const metadata = await lstat(path);
  if (metadata.isSymbolicLink() || !metadata.isFile()) {
    throw new Error("Generated icon.icns must be a regular non-symlink file");
  }
  const bytes = await readFile(path);
  if (bytes.length < 8 || bytes.subarray(0, 4).toString("ascii") !== "icns") {
    throw new Error("Generated icon.icns has an invalid header");
  }
  if (bytes.readUInt32BE(4) !== bytes.length) {
    throw new Error("Generated icon.icns has an invalid declared size");
  }

  const chunks = [];
  for (let offset = 8; offset < bytes.length;) {
    if (offset + 8 > bytes.length) throw new Error("Generated icon.icns has a truncated chunk");
    const size = bytes.readUInt32BE(offset + 4);
    if (size < 8 || offset + size > bytes.length) {
      throw new Error("Generated icon.icns has invalid chunk bounds");
    }
    chunks.push(Buffer.from(bytes.subarray(offset, offset + size)));
    offset += size;
  }
  chunks.sort((first, second) => {
    const typeOrder = Buffer.compare(first.subarray(0, 4), second.subarray(0, 4));
    return typeOrder || Buffer.compare(first, second);
  });

  const canonical = Buffer.concat([bytes.subarray(0, 8), ...chunks]);
  if (Buffer.compare(bytes, canonical) === 0) return;
  const temporary = `${path}.${randomUUID()}.partial`;
  try {
    await writeFile(temporary, canonical, { flag: "wx", mode: metadata.mode });
    await rename(temporary, path);
  } finally {
    await rm(temporary, { force: true });
  }
};

const outputDirectory = parseOutputDirectory(process.argv.slice(2));
const result = spawnSync(
  npm,
  [
    "run",
    "--silent",
    "tauri",
    "--",
    "icon",
    "src-tauri/icons/wakegpt-icon.svg",
    "--output",
    outputDirectory,
  ],
  { cwd: appRoot, stdio: "inherit" },
);
if (result.status !== 0) process.exit(result.status ?? 1);
await canonicalizeIcns(join(outputDirectory, "icon.icns"));
