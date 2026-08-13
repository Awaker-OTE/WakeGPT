import { createHash, randomUUID } from "node:crypto";
import { constants } from "node:fs";
import { lstat, mkdir, open, rename, rm } from "node:fs/promises";
import { basename, dirname, resolve } from "node:path";

const parseArguments = (arguments_) => {
  let output = null;
  const inputs = [];
  for (let index = 0; index < arguments_.length; index += 1) {
    const argument = arguments_[index];
    if (argument === "--output") {
      if (output !== null) throw new Error("--output may be provided only once");
      const value = arguments_[index + 1];
      if (!value) throw new Error("--output requires a file path");
      output = resolve(value);
      index += 1;
    } else if (argument.startsWith("--")) {
      throw new Error(`Unknown argument: ${argument}`);
    } else {
      inputs.push(resolve(argument));
    }
  }
  if (!output) throw new Error("--output is required");
  if (inputs.length === 0) throw new Error("At least one input file is required");
  return { output, inputs };
};

const sameFileIdentity = (first, second) => (
  first.dev === second.dev
  && first.ino === second.ino
  && first.size === second.size
  && first.mtimeMs === second.mtimeMs
);

const hashStableFile = async (path) => {
  const pathBefore = await lstat(path);
  if (pathBefore.isSymbolicLink() || !pathBefore.isFile()) {
    throw new Error(`${basename(path)} must be a regular non-symlink file`);
  }
  const handle = await open(path, constants.O_RDONLY);
  try {
    const openedBefore = await handle.stat();
    if (!sameFileIdentity(pathBefore, openedBefore)) {
      throw new Error(`${basename(path)} changed before hashing`);
    }
    const hash = createHash("sha256");
    const buffer = Buffer.allocUnsafe(64 * 1024);
    let position = 0;
    while (true) {
      const { bytesRead } = await handle.read(buffer, 0, buffer.length, position);
      if (bytesRead === 0) break;
      hash.update(buffer.subarray(0, bytesRead));
      position += bytesRead;
    }
    const openedAfter = await handle.stat();
    const pathAfter = await lstat(path);
    if (!sameFileIdentity(openedBefore, openedAfter) || !sameFileIdentity(openedAfter, pathAfter)) {
      throw new Error(`${basename(path)} changed while hashing`);
    }
    return hash.digest("hex");
  } finally {
    await handle.close();
  }
};

const { output, inputs } = parseArguments(process.argv.slice(2));
const outputName = basename(output);
const namedInputs = inputs.map((path) => ({ path, name: basename(path) }));
if (namedInputs.some((input) => input.path === output)) {
  throw new Error("The checksum output cannot also be an input");
}
if (new Set(namedInputs.map((input) => input.name)).size !== namedInputs.length) {
  throw new Error("Input basenames must be unique");
}
namedInputs.sort((first, second) => first.name.localeCompare(second.name));

const lines = [];
for (const input of namedInputs) {
  lines.push(`${await hashStableFile(input.path)}  ${input.name}`);
}
const contents = `${lines.join("\n")}\n`;

await mkdir(dirname(output), { recursive: true });
try {
  const existing = await lstat(output);
  if (existing.isSymbolicLink() || !existing.isFile()) {
    throw new Error(`${outputName} must be absent or a regular non-symlink file`);
  }
} catch (error) {
  if (error?.code !== "ENOENT") throw error;
}

const temporary = `${output}.${randomUUID()}.partial`;
try {
  const handle = await open(temporary, constants.O_CREAT | constants.O_EXCL | constants.O_WRONLY, 0o644);
  try {
    await handle.writeFile(contents, "utf8");
    await handle.sync();
  } finally {
    await handle.close();
  }
  await rename(temporary, output);
  const directory = await open(dirname(output), constants.O_RDONLY);
  try {
    await directory.sync();
  } finally {
    await directory.close();
  }
} finally {
  await rm(temporary, { force: true });
}

console.log(JSON.stringify({ output: outputName, fileCount: namedInputs.length }));
