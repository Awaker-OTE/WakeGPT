import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmod, mkdir, mkdtemp, rm, symlink, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";

const appRoot = fileURLToPath(new URL("..", import.meta.url));
const packager = fileURLToPath(new URL("./package-macos-dmg.mjs", import.meta.url));

if (process.platform !== "darwin") {
  console.log("macOS post-sign DMG contract: skipped outside macOS");
  process.exit(0);
}

const root = await mkdtemp(join(tmpdir(), "wakegpt-macos-dmg-test-"));
const app = join(root, "WakeGPT.app");
const executableDirectory = join(app, "Contents", "MacOS");
const executable = join(executableDirectory, "wakegpt-desktop");
const output = join(root, "WakeGPT_0.1.0_universal.dmg");

const run = (arguments_, expectedStatus = 0) => {
  const result = spawnSync(process.execPath, [packager, ...arguments_], {
    cwd: appRoot,
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
  });
  assert.equal(result.status, expectedStatus, result.stderr || result.stdout);
  return result;
};

try {
  await mkdir(executableDirectory, { recursive: true });
  const source = "int main(void) { return 0; }\n";
  const arm64 = join(root, "wakegpt-arm64");
  const x86_64 = join(root, "wakegpt-x86_64");
  for (const [architecture, architectureOutput] of [["arm64", arm64], ["x86_64", x86_64]]) {
    const compiled = spawnSync(
      "/usr/bin/clang",
      ["-arch", architecture, "-x", "c", "-", "-o", architectureOutput],
      { input: source, encoding: "utf8" },
    );
    assert.equal(compiled.status, 0, compiled.stderr || compiled.stdout);
  }
  const merged = spawnSync("/usr/bin/lipo", ["-create", arm64, x86_64, "-output", executable], {
    encoding: "utf8",
  });
  assert.equal(merged.status, 0, merged.stderr || merged.stdout);
  await chmod(executable, 0o755);
  await writeFile(join(app, "Contents", "Info.plist"), `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>wakegpt-desktop</string>
<key>CFBundleIdentifier</key><string>com.wakegpt.desktop</string>
<key>CFBundleName</key><string>WakeGPT</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>CFBundleVersion</key><string>1</string>
</dict></plist>
`, "utf8");
  const signed = spawnSync("codesign", [
    "--force",
    "--deep",
    "--options", "runtime",
    "--timestamp=none",
    "--sign", "-",
    app,
  ], { encoding: "utf8" });
  assert.equal(signed.status, 0, signed.stderr || signed.stdout);

  const missingMode = run([
    "--app", app,
    "--output", output,
    "--volume-name", "WakeGPT",
  ], 1);
  assert.match(missingMode.stderr, /--release-mode is required/u);

  const invalidMode = run([
    "--app", app,
    "--output", output,
    "--release-mode", "unsigned",
    "--volume-name", "WakeGPT",
  ], 1);
  assert.match(invalidMode.stderr, /--release-mode must be one of/u);

  const developerIdArguments = [
    "--app", app,
    "--output", output,
    "--release-mode", "developer-id-notarized",
    "--volume-name", "WakeGPT",
  ];
  const rejected = run(developerIdArguments, 1);
  assert.match(rejected.stderr, /Developer ID Application/u);

  const baseArguments = [
    "--app", app,
    "--output", output,
    "--release-mode", "adhoc-public-unnotarized",
    "--volume-name", "WakeGPT",
  ];

  const packaged = run(baseArguments);
  const report = JSON.parse(packaged.stdout.trim());
  assert.equal(report.file, "WakeGPT_0.1.0_universal.dmg");
  assert.match(report.sha256, /^[a-f0-9]{64}$/u);
  assert.equal(report.mode, "adhoc-public-unnotarized");
  const dmgSignature = spawnSync("codesign", ["-dv", "--verbose=4", output], { encoding: "utf8" });
  assert.equal(dmgSignature.status, 0, dmgSignature.stderr || dmgSignature.stdout);
  assert.match(`${dmgSignature.stdout}${dmgSignature.stderr}`, /^Identifier=com\.wakegpt\.desktop\.dmg$/mu);
  run(baseArguments, 1);

  const linkedDirectory = join(root, "linked");
  await mkdir(linkedDirectory);
  await symlink(app, join(linkedDirectory, "WakeGPT.app"));
  const linkedOutput = join(root, "linked.dmg");
  const linked = run([
    "--app",
    join(linkedDirectory, "WakeGPT.app"),
    "--output",
    linkedOutput,
    "--release-mode", "adhoc-public-unnotarized",
    "--volume-name", "WakeGPT",
  ], 1);
  assert.match(linked.stderr, /non-symlink directory/u);
} finally {
  await rm(root, { recursive: true, force: true });
}

console.log("macOS post-sign DMG contract: ok");
