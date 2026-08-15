import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  chmod,
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
const packager = fileURLToPath(new URL("./package-macos-update.mjs", import.meta.url));
const tauri = join(appRoot, "node_modules", ".bin", process.platform === "win32" ? "tauri.cmd" : "tauri");
const projectVersion = JSON.parse(await readFile(join(appRoot, "package.json"), "utf8")).version;
const keyPathVariable = ["TAURI", "SIGNING", "PRIVATE", "KEY", "PATH"].join("_");
const keyValueVariable = ["TAURI", "SIGNING", "PRIVATE", "KEY"].join("_");
const keyPasswordVariable = ["TAURI", "SIGNING", "PRIVATE", "KEY", "PASSWORD"].join("_");
const signerTestPhrase = ["wakegpt", "temporary", "updater", "fixture"].join("-");

if (process.platform !== "darwin") {
  console.log("macOS signed updater package contract: skipped outside macOS");
  process.exit(0);
}

const root = await mkdtemp(join(tmpdir(), "wakegpt-macos-update-test-"));
const app = join(root, "WakeGPT.app");
const contents = join(app, "Contents");
const executableDirectory = join(contents, "MacOS");
const executable = join(executableDirectory, "wakegpt-desktop");
const privateKey = join(root, "update.key");
const publicKey = `${privateKey}.pub`;
const notes = join(root, "release-notes.md");

const cleanEnvironment = () => Object.fromEntries(
  Object.entries(process.env).filter(([name]) => ![
    keyPathVariable,
    keyValueVariable,
    keyPasswordVariable,
  ].includes(name)),
);

const runTauri = (arguments_) => {
  const result = spawnSync(tauri, arguments_, {
    cwd: appRoot,
    encoding: "utf8",
    env: cleanEnvironment(),
    maxBuffer: 16 * 1024 * 1024,
  });
  assert.equal(result.status, 0, result.stderr || result.stdout);
};

const run = (arguments_, expectedStatus = 0, environment = {}) => {
  const result = spawnSync(process.execPath, [packager, ...arguments_], {
    cwd: appRoot,
    encoding: "utf8",
    env: { ...cleanEnvironment(), ...environment },
    maxBuffer: 32 * 1024 * 1024,
    timeout: 5 * 60_000,
  });
  assert.equal(result.status, expectedStatus, result.stderr || result.stdout);
  return result;
};

const packageArguments = (
  outputDirectory,
  appPath = app,
  releaseMode = "adhoc-public-unnotarized",
) => [
  "--app", appPath,
  "--output-dir", outputDirectory,
  "--version", projectVersion,
  "--repository", "wakegpt/wakegpt",
  "--public-key", publicKey,
  "--pub-date", "2026-08-12T00:00:00Z",
  "--release-mode", releaseMode,
  "--notes", notes,
  "--source-date-epoch", "1786406400",
];

const signingEnvironment = (phrase = signerTestPhrase) => ({
  [keyPathVariable]: privateKey,
  [keyPasswordVariable]: phrase,
});

try {
  await mkdir(executableDirectory, { recursive: true });
  const source = "int main(void) { return 0; }\n";
  const arm64 = join(root, "wakegpt-arm64");
  const x86_64 = join(root, "wakegpt-x86_64");
  for (const [architecture, output] of [["arm64", arm64], ["x86_64", x86_64]]) {
    const compiled = spawnSync("/usr/bin/clang", ["-arch", architecture, "-x", "c", "-", "-o", output], {
      input: source,
      encoding: "utf8",
    });
    assert.equal(compiled.status, 0, compiled.stderr || compiled.stdout);
  }
  const merged = spawnSync("/usr/bin/lipo", ["-create", arm64, x86_64, "-output", executable], {
    encoding: "utf8",
  });
  assert.equal(merged.status, 0, merged.stderr || merged.stdout);
  await chmod(executable, 0o755);
  await writeFile(join(contents, "Info.plist"), `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>wakegpt-desktop</string>
<key>CFBundleIdentifier</key><string>com.wakegpt.desktop</string>
<key>CFBundleName</key><string>WakeGPT</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>${projectVersion}</string>
<key>CFBundleVersion</key><string>${projectVersion}</string>
</dict></plist>
`, "utf8");
  const signed = spawnSync("/usr/bin/codesign", [
    "--force",
    "--deep",
    "--options", "runtime",
    "--timestamp=none",
    "--sign", "-",
    app,
  ], { encoding: "utf8" });
  assert.equal(signed.status, 0, signed.stderr || signed.stdout);

  runTauri(["signer", "generate", "--ci", "--password", signerTestPhrase, "--write-keys", privateKey]);
  await chmod(privateKey, 0o600);
  await writeFile(notes, "## 中文\n\n安全更新。\n\n## English\n\nSecurity update.\n", "utf8");

  const missingModeArguments = packageArguments(join(root, "missing-mode"));
  missingModeArguments.splice(missingModeArguments.indexOf("--release-mode"), 2);
  const missingMode = run(missingModeArguments, 1, signingEnvironment());
  assert.match(missingMode.stderr, /--release-mode is required/u);

  const invalidModeArguments = packageArguments(join(root, "invalid-mode"));
  invalidModeArguments[invalidModeArguments.indexOf("--release-mode") + 1] = "unsigned";
  const invalidMode = run(invalidModeArguments, 1, signingEnvironment());
  assert.match(invalidMode.stderr, /--release-mode must be one of/u);

  const formal = run(
    packageArguments(join(root, "formal"), app, "developer-id-notarized"),
    1,
    signingEnvironment(),
  );
  assert.match(formal.stderr, /Developer ID Application/u);

  const missingKey = run(packageArguments(join(root, "missing-key")), 1);
  assert.match(missingKey.stderr, /exactly one updater private key/u);

  const firstDirectory = join(root, "first-public");
  const secondDirectory = join(root, "second-public");
  const first = run(packageArguments(firstDirectory), 0, signingEnvironment());
  const second = run(packageArguments(secondDirectory), 0, signingEnvironment());
  const firstReport = JSON.parse(first.stdout.trim());
  assert.equal(firstReport.mode, "adhoc-public-unnotarized");
  assert.equal(firstReport.version, projectVersion);
  assert.equal(firstReport.target, "darwin-universal");
  assert.match(firstReport.files.archive.sha256, /^[a-f0-9]{64}$/u);
  assert.deepEqual(
    await readFile(join(firstDirectory, "WakeGPT_universal.app.tar.gz")),
    await readFile(join(secondDirectory, "WakeGPT_universal.app.tar.gz")),
    "normalized update archives must be byte reproducible",
  );
  const manifest = JSON.parse(await readFile(join(firstDirectory, "latest-darwin-universal.json"), "utf8"));
  assert.equal(manifest.version, projectVersion);
  assert.equal(
    manifest.platforms["darwin-universal"].signature,
    (await readFile(join(firstDirectory, "WakeGPT_universal.app.tar.gz.sig"), "utf8")).trim(),
  );

  const extracted = join(root, "extracted");
  await mkdir(extracted);
  const extraction = spawnSync("/usr/bin/tar", [
    "-xzf",
    join(firstDirectory, "WakeGPT_universal.app.tar.gz"),
    "-C",
    extracted,
  ], { encoding: "utf8" });
  assert.equal(extraction.status, 0, extraction.stderr || extraction.stdout);
  const verified = spawnSync("/usr/bin/codesign", [
    "--verify",
    "--deep",
    "--strict",
    join(extracted, "WakeGPT.app"),
  ], { encoding: "utf8" });
  assert.equal(verified.status, 0, verified.stderr || verified.stdout);
  const compared = spawnSync("/usr/bin/diff", ["-rq", app, join(extracted, "WakeGPT.app")], {
    encoding: "utf8",
  });
  assert.equal(compared.status, 0, compared.stderr || compared.stdout);

  const collision = run(packageArguments(firstDirectory), 1, signingEnvironment());
  assert.match(collision.stderr, /already exists/u);

  const linkedAppDirectory = join(root, "linked-app");
  await mkdir(linkedAppDirectory);
  const linkedApp = join(linkedAppDirectory, "WakeGPT.app");
  await symlink(app, linkedApp);
  const linked = run(
    packageArguments(join(root, "linked-app-output"), linkedApp),
    1,
    signingEnvironment(),
  );
  assert.match(linked.stderr, /non-symlink directory/u);

  const linkedKey = join(root, "linked-update.key");
  await symlink(privateKey, linkedKey);
  const linkedKeyResult = run(packageArguments(join(root, "linked-key")), 1, {
    [keyPathVariable]: linkedKey,
    [keyPasswordVariable]: signerTestPhrase,
  });
  assert.match(linkedKeyResult.stderr, /non-symlink file/u);

  const wrongPassword = ["wrong", "temporary", "phrase"].join("-");
  const signingFailure = run(
    packageArguments(join(root, "signing-failure")),
    1,
    signingEnvironment(wrongPassword),
  );
  assert.match(signingFailure.stderr, /Tauri updater signing failed/u);
  assert.doesNotMatch(signingFailure.stderr, new RegExp(signerTestPhrase, "u"));
  assert.doesNotMatch(signingFailure.stderr, new RegExp(privateKey.replaceAll("/", "\\/"), "u"));
} finally {
  await rm(root, { recursive: true, force: true });
}

console.log("macOS signed updater package contract: ok");
