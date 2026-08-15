import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  appendFile,
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";

const appRoot = fileURLToPath(new URL("..", import.meta.url));
const generator = fileURLToPath(new URL("./generate-update-manifest.mjs", import.meta.url));
const tauri = join(appRoot, "node_modules", ".bin", process.platform === "win32" ? "tauri.cmd" : "tauri");
const projectVersion = JSON.parse(await readFile(join(appRoot, "package.json"), "utf8")).version;
const tar = process.platform === "darwin" ? "/usr/bin/tar" : "tar";
const root = await mkdtemp(join(tmpdir(), "wakegpt-update-manifest-"));
const signerTestPhrase = ["wakegpt", "ephemeral", "test", "key"].join("-");

const run = (arguments_, expectedStatus = 0) => {
  const result = spawnSync(process.execPath, [generator, ...arguments_], {
    cwd: appRoot,
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
  });
  assert.equal(result.status, expectedStatus, result.stderr || result.stdout);
  return result;
};

const runTauri = (arguments_) => {
  const result = spawnSync(tauri, arguments_, {
    cwd: appRoot,
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
  });
  assert.equal(result.status, 0, result.stderr || result.stdout);
};

const createKey = (path) => {
  runTauri(["signer", "generate", "--ci", "--password", signerTestPhrase, "--write-keys", path]);
};

try {
  const staging = join(root, "staging");
  const app = join(staging, "WakeGPT.app");
  const executable = join(app, "Contents", "MacOS", "wakegpt-desktop");
  await mkdir(join(app, "Contents", "MacOS"), { recursive: true });
  await writeFile(join(app, "Contents", "Info.plist"), `<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.wakegpt.desktop</string>
<key>CFBundleExecutable</key><string>wakegpt-desktop</string>
<key>CFBundleShortVersionString</key><string>${projectVersion}</string>
<key>CFBundleVersion</key><string>${projectVersion}</string>
</dict></plist>
`, "utf8");
  await writeFile(executable, "synthetic universal executable\n", "utf8");

  const bundle = join(root, "WakeGPT_universal.app.tar.gz");
  const tarResult = spawnSync(tar, ["-czf", bundle, "-C", staging, "WakeGPT.app"], {
    encoding: "utf8",
  });
  assert.equal(tarResult.status, 0, tarResult.stderr || tarResult.stdout);

  const privateKey = join(root, "update.key");
  const publicKey = `${privateKey}.pub`;
  createKey(privateKey);
  runTauri([
    "signer",
    "sign",
    "--private-key-path",
    privateKey,
    "--password",
    signerTestPhrase,
    bundle,
  ]);
  const signature = `${bundle}.sig`;
  const notes = join(root, "release-notes.md");
  await writeFile(notes, "## 中文\r\n\r\n安全更新。\r\n\r\n## English\r\n\r\nSecurity update.\r\n", "utf8");

  const argumentsFor = ({
    output,
    bundlePath = bundle,
    signaturePath = signature,
    publicKeyPath = publicKey,
    notesPath = notes,
    version = projectVersion,
    repository = "wakegpt/wakegpt",
    target = "darwin-universal",
    pubDate = "2026-08-12T00:00:00Z",
  }) => [
    "--version", version,
    "--repository", repository,
    "--target", target,
    "--bundle", bundlePath,
    "--signature", signaturePath,
    "--public-key", publicKeyPath,
    "--pub-date", pubDate,
    "--notes", notesPath,
    "--output", output,
  ];

  const firstDirectory = join(root, "first");
  const secondDirectory = join(root, "second");
  const firstOutput = join(firstDirectory, "latest-darwin-universal.json");
  const secondOutput = join(secondDirectory, "latest-darwin-universal.json");
  run(argumentsFor({ output: firstOutput }));
  run(argumentsFor({ output: secondOutput }));
  const firstBytes = await readFile(firstOutput, "utf8");
  assert.equal(await readFile(secondOutput, "utf8"), firstBytes, "manifest output must be reproducible");
  assert.doesNotMatch(firstBytes, /(?:\/tmp\/|\/private\/tmp\/|\/Users\/|[A-Za-z]:\\Users\\)/u);
  const manifest = JSON.parse(firstBytes);
  assert.deepEqual(Object.keys(manifest.platforms), ["darwin-universal"]);
  assert.equal(manifest.version, projectVersion);
  assert.equal(manifest.pub_date, "2026-08-12T00:00:00Z");
  assert.equal(manifest.notes, "## 中文\n\n安全更新。\n\n## English\n\nSecurity update.");
  assert.equal(
    manifest.platforms["darwin-universal"].url,
    `https://github.com/wakegpt/wakegpt/releases/download/v${projectVersion}/WakeGPT_universal.app.tar.gz`,
  );
  assert.equal(
    manifest.platforms["darwin-universal"].signature,
    (await readFile(signature, "utf8")).trim(),
  );

  run(argumentsFor({ output: firstOutput }), 1);

  const tamperedDirectory = join(root, "tampered");
  await mkdir(tamperedDirectory);
  const tamperedBundle = join(tamperedDirectory, "WakeGPT_universal.app.tar.gz");
  const tamperedSignature = `${tamperedBundle}.sig`;
  await copyFile(bundle, tamperedBundle);
  await copyFile(signature, tamperedSignature);
  await appendFile(tamperedBundle, "tampered", "utf8");
  const tamperedResult = run(argumentsFor({
    output: join(tamperedDirectory, "latest-darwin-universal.json"),
    bundlePath: tamperedBundle,
    signaturePath: tamperedSignature,
  }), 1);
  assert.match(tamperedResult.stderr, /signature verification failed/u);

  const otherKey = join(root, "other.key");
  createKey(otherKey);
  const wrongKeyResult = run(argumentsFor({
    output: join(root, "wrong-key", "latest-darwin-universal.json"),
    publicKeyPath: `${otherKey}.pub`,
  }), 1);
  assert.match(wrongKeyResult.stderr, /key ID does not match/u);

  const invalidArchiveDirectory = join(root, "invalid-archive");
  await mkdir(invalidArchiveDirectory);
  const invalidArchive = join(invalidArchiveDirectory, "WakeGPT_universal.app.tar.gz");
  await writeFile(invalidArchive, "not a tar archive\n", "utf8");
  runTauri([
    "signer",
    "sign",
    "--private-key-path",
    privateKey,
    "--password",
    signerTestPhrase,
    invalidArchive,
  ]);
  const invalidArchiveResult = run(argumentsFor({
    output: join(invalidArchiveDirectory, "latest-darwin-universal.json"),
    bundlePath: invalidArchive,
    signaturePath: `${invalidArchive}.sig`,
  }), 1);
  assert.match(invalidArchiveResult.stderr, /readable gzip-compressed tar archive/u);

  const linkedDirectory = join(root, "linked");
  await mkdir(linkedDirectory);
  const linkedBundle = join(linkedDirectory, "WakeGPT_universal.app.tar.gz");
  const linkedSignature = `${linkedBundle}.sig`;
  try {
    await symlink(bundle, linkedBundle);
    await copyFile(signature, linkedSignature);
    run(argumentsFor({
      output: join(linkedDirectory, "latest-darwin-universal.json"),
      bundlePath: linkedBundle,
      signaturePath: linkedSignature,
    }), 1);
  } catch (error) {
    if (error?.code !== "EPERM") throw error;
  }

  const emptyNotes = join(root, "empty-notes.md");
  await writeFile(emptyNotes, "\n", "utf8");
  run(argumentsFor({
    output: join(root, "empty", "latest-darwin-universal.json"),
    notesPath: emptyNotes,
  }), 1);
  run(argumentsFor({
    output: join(root, "bad-version", "latest-darwin-universal.json"),
    version: "v0.1.0",
  }), 1);
  const mismatchedVersionResult = run(argumentsFor({
    output: join(root, "mismatched-version", "latest-darwin-universal.json"),
    version: "9.9.9",
  }), 1);
  assert.match(mismatchedVersionResult.stderr, /must match package\.json/u);
  run(argumentsFor({
    output: join(root, "bad-repository", "latest-darwin-universal.json"),
    repository: "https://github.com/wakegpt/wakegpt",
  }), 1);
  run(argumentsFor({
    output: join(root, "bad-target", "latest-darwin-universal.json"),
    target: "darwin-aarch64",
  }), 1);
  run(argumentsFor({
    output: join(root, "bad-date", "latest-darwin-universal.json"),
    pubDate: "2026-02-30T00:00:00Z",
  }), 1);
  const wrongBundleName = join(root, "WakeGPT.app.tar.gz");
  await copyFile(bundle, wrongBundleName);
  await copyFile(signature, `${wrongBundleName}.sig`);
  run(argumentsFor({
    output: join(root, "bad-name", "latest-darwin-universal.json"),
    bundlePath: wrongBundleName,
    signaturePath: `${wrongBundleName}.sig`,
  }), 1);
} finally {
  await rm(root, { recursive: true, force: true });
}

console.log("signed update manifest contract: ok");
