import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";

const appRoot = fileURLToPath(new URL("..", import.meta.url));
const generator = fileURLToPath(new URL("./generate-release-checksums.mjs", import.meta.url));
const root = await mkdtemp(join(tmpdir(), "wakegpt-release-checksums-"));

const run = (arguments_, expectedStatus = 0) => {
  const result = spawnSync(process.execPath, [generator, ...arguments_], {
    cwd: appRoot,
    encoding: "utf8",
  });
  assert.equal(result.status, expectedStatus, result.stderr || result.stdout);
  return result;
};

const digest = (value) => createHash("sha256").update(value).digest("hex");

try {
  const first = join(root, "wakegpt-0.1.0.cdx.json");
  const second = join(root, "WakeGPT_0.1.0_aarch64.dmg");
  const output = join(root, "SHA256SUMS.txt");
  await writeFile(first, "sbom\n", "utf8");
  await writeFile(second, "dmg\n", "utf8");

  run(["--output", output, second, first]);
  const expected = [
    `${digest("dmg\n")}  WakeGPT_0.1.0_aarch64.dmg`,
    `${digest("sbom\n")}  wakegpt-0.1.0.cdx.json`,
    "",
  ].join("\n");
  const firstOutput = await readFile(output, "utf8");
  assert.equal(firstOutput, expected, "checksums must use stable basename order and standard spacing");
  assert.doesNotMatch(firstOutput, /(?:\/Users\/|\/tmp\/|[A-Za-z]:\\Users\\)/u);
  run(["--output", output, second, first]);
  assert.equal(await readFile(output, "utf8"), firstOutput, "repeated output must be byte-identical");

  const duplicateDirectory = join(root, "duplicate");
  await mkdir(duplicateDirectory);
  const duplicate = join(duplicateDirectory, "wakegpt-0.1.0.cdx.json");
  await writeFile(duplicate, "different\n", "utf8");
  run(["--output", output, first, duplicate], 1);
  run(["--output", output, output], 1);

  const link = join(root, "linked.dmg");
  try {
    await symlink(second, link);
    run(["--output", output, link], 1);
  } catch (error) {
    if (error?.code !== "EPERM") throw error;
  }
} finally {
  await rm(root, { recursive: true, force: true });
}

console.log("release checksum contract: ok");
