import { createHash, randomUUID } from "node:crypto";
import { spawnSync } from "node:child_process";
import { createReadStream } from "node:fs";
import {
  link,
  lstat,
  mkdir,
  mkdtemp,
  open,
  rm,
  symlink,
} from "node:fs/promises";
import { basename, dirname, extname, join, resolve } from "node:path";
import { tmpdir } from "node:os";

const EXPECTED_BUNDLE_ID = "com.wakegpt.desktop";
const EXPECTED_DMG_ID = `${EXPECTED_BUNDLE_ID}.dmg`;
const RELEASE_MODES = new Set([
  "adhoc-public-unnotarized",
  "developer-id-notarized",
]);

const parseArguments = (arguments_) => {
  const values = new Map();
  for (let index = 0; index < arguments_.length; index += 1) {
    const argument = arguments_[index];
    if (!["--app", "--output", "--release-mode", "--volume-name"].includes(argument)) {
      throw new Error(`Unknown argument: ${argument}`);
    }
    if (values.has(argument)) throw new Error(`${argument} may be provided only once`);
    const value = arguments_[index + 1];
    if (!value || value.startsWith("--")) throw new Error(`${argument} requires a value`);
    values.set(argument, value);
    index += 1;
  }
  for (const required of ["--app", "--output", "--release-mode", "--volume-name"]) {
    if (!values.has(required)) throw new Error(`${required} is required`);
  }
  const volumeName = values.get("--volume-name");
  if (!/^[A-Za-z0-9][A-Za-z0-9 ._-]{0,26}$/u.test(volumeName)) {
    throw new Error("--volume-name must be 1-27 portable characters");
  }
  const app = resolve(values.get("--app"));
  const output = resolve(values.get("--output"));
  const releaseMode = values.get("--release-mode");
  if (basename(app) !== "WakeGPT.app") throw new Error("--app must identify WakeGPT.app");
  if (extname(output).toLowerCase() !== ".dmg") throw new Error("--output must end in .dmg");
  if (!RELEASE_MODES.has(releaseMode)) {
    throw new Error(`--release-mode must be one of: ${[...RELEASE_MODES].join(", ")}`);
  }
  return { app, output, releaseMode, volumeName };
};

const run = (command, arguments_, options = {}) => {
  const result = spawn(command, arguments_, options);
  if (result.status !== 0) {
    const detail = `${result.stderr || result.stdout || ""}`.trim();
    throw new Error(`${command} failed${detail ? `: ${detail}` : ""}`);
  }
  return `${result.stdout || ""}${result.stderr || ""}`;
};

const spawn = (command, arguments_, options = {}) => {
  return spawnSync(command, arguments_, {
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
    ...options,
  });
};

const commandOutput = (command, arguments_) => run(command, arguments_).trim();

const validatePathType = async (path, expected, label) => {
  const metadata = await lstat(path);
  if (metadata.isSymbolicLink() || !metadata[expected]()) {
    throw new Error(`${label} must be a non-symlink ${expected === "isDirectory" ? "directory" : "file"}`);
  }
};

const verifyApp = async (app, releaseMode) => {
  await validatePathType(app, "isDirectory", "WakeGPT.app");
  run("codesign", ["--verify", "--deep", "--strict", "--verbose=2", app]);
  const signature = run("codesign", ["-dv", "--verbose=4", app]);
  if (!/flags=.*\bruntime\b/mu.test(signature)) {
    throw new Error("WakeGPT.app must enable the hardened runtime");
  }
  if (releaseMode === "adhoc-public-unnotarized") {
    if (!/^Signature=adhoc$/mu.test(signature)) {
      throw new Error("WakeGPT.app must use a complete ad-hoc signature for this release mode");
    }
  } else {
    if (!/^Authority=Developer ID Application:/mu.test(signature)) {
      throw new Error("WakeGPT.app must use a Developer ID Application signature");
    }
    if (!/^TeamIdentifier=\S+/mu.test(signature) || !/^Timestamp=\S+/mu.test(signature)) {
      throw new Error("WakeGPT.app must have a timestamped Developer ID signature");
    }
    run("xcrun", ["stapler", "validate", app]);
  }
  const info = join(app, "Contents", "Info.plist");
  await validatePathType(info, "isFile", "Info.plist");
  const bundleId = commandOutput("plutil", ["-extract", "CFBundleIdentifier", "raw", "-o", "-", info]);
  if (bundleId !== EXPECTED_BUNDLE_ID) throw new Error("WakeGPT.app has an unexpected bundle identifier");
  const executableName = commandOutput("plutil", ["-extract", "CFBundleExecutable", "raw", "-o", "-", info]);
  const executable = join(app, "Contents", "MacOS", executableName);
  await validatePathType(executable, "isFile", "WakeGPT executable");
  const architectures = new Set(commandOutput("lipo", ["-archs", executable]).split(/\s+/u));
  if (
    architectures.size !== 2
    || !architectures.has("arm64")
    || !architectures.has("x86_64")
  ) {
    throw new Error("WakeGPT.app executable must contain exactly arm64 and x86_64 slices");
  }
  const version = commandOutput("plutil", ["-extract", "CFBundleShortVersionString", "raw", "-o", "-", info]);
  return { executable, signature, version };
};

const sha256 = async (path) => {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest("hex");
};

const syncFileAndDirectory = async (path) => {
  const file = await open(path, "r");
  try {
    await file.sync();
  } finally {
    await file.close();
  }
  const directory = await open(dirname(path), "r");
  try {
    await directory.sync();
  } finally {
    await directory.close();
  }
};

const main = async () => {
  if (process.platform !== "darwin") throw new Error("macOS DMG packaging is supported only on macOS");
  const { app, output, releaseMode, volumeName } = parseArguments(process.argv.slice(2));
  const isAdhoc = releaseMode === "adhoc-public-unnotarized";
  const outputParent = dirname(output);
  await mkdir(outputParent, { recursive: true });
  await validatePathType(outputParent, "isDirectory", "DMG output directory");
  try {
    await lstat(output);
    throw new Error("DMG output already exists");
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }

  const source = await verifyApp(app, releaseMode);
  const signingIdentity = isAdhoc ? "-" : `${process.env.APPLE_SIGNING_IDENTITY || ""}`.trim();
  if (!signingIdentity) throw new Error("APPLE_SIGNING_IDENTITY is required for a release DMG");

  const work = await mkdtemp(join(tmpdir(), "wakegpt-macos-dmg-"));
  const staging = join(work, "staging");
  const mounted = join(work, "mounted");
  const stagedApp = join(staging, "WakeGPT.app");
  const partial = join(outputParent, `.${basename(output)}.${randomUUID()}.partial.dmg`);
  let attached = false;
  try {
    await mkdir(staging);
    await mkdir(mounted);
    run("ditto", [app, stagedApp]);
    run("diff", ["-rq", app, stagedApp]);
    await verifyApp(stagedApp, releaseMode);
    await symlink("/Applications", join(staging, "Applications"));

    run("hdiutil", [
      "create",
      "-srcfolder",
      staging,
      "-format",
      "UDZO",
      "-fs",
      "HFS+",
      "-volname",
      volumeName,
      "-nospotlight",
      partial,
    ]);
    const signArguments = [
      "--force",
      "--identifier", EXPECTED_DMG_ID,
      "--sign", signingIdentity,
    ];
    signArguments.push(isAdhoc ? "--timestamp=none" : "--timestamp", partial);
    run("codesign", signArguments);
    run("codesign", ["--verify", "--strict", "--verbose=2", partial]);
    const dmgSignature = run("codesign", ["-dv", "--verbose=4", partial]);
    if (!new RegExp(`^Identifier=${EXPECTED_DMG_ID.replaceAll(".", "\\.")}$`, "mu").test(dmgSignature)) {
      throw new Error("DMG has an unexpected code-signing identifier");
    }
    if (isAdhoc) {
      if (!/^Signature=adhoc$/mu.test(dmgSignature)) {
        throw new Error("DMG must use an ad-hoc signature for this release mode");
      }
    } else {
      if (!/^Authority=Developer ID Application:/mu.test(dmgSignature)) {
        throw new Error("DMG must use a Developer ID Application signature");
      }
      const appTeam = source.signature.match(/^TeamIdentifier=(.+)$/mu)?.[1];
      const dmgTeam = dmgSignature.match(/^TeamIdentifier=(.+)$/mu)?.[1];
      if (!appTeam || !dmgTeam || appTeam !== dmgTeam) {
        throw new Error("DMG and WakeGPT.app must use the same Developer ID team");
      }
    }
    run("hdiutil", ["verify", partial]);
    run("hdiutil", [
      "attach",
      partial,
      "-readonly",
      "-nobrowse",
      "-noautoopen",
      "-mountpoint",
      mounted,
    ]);
    attached = true;
    const mountedApp = join(mounted, "WakeGPT.app");
    const mountedState = await verifyApp(mountedApp, releaseMode);
    run("diff", ["-rq", app, mountedApp]);
    if (await sha256(source.executable) !== await sha256(mountedState.executable)) {
      throw new Error("Mounted DMG executable does not match the signed source app");
    }
    run("hdiutil", ["detach", mounted]);
    attached = false;

    await syncFileAndDirectory(partial);
    await link(partial, output);
    await rm(partial);
    await syncFileAndDirectory(output);
    console.log(JSON.stringify({
      file: basename(output),
      sha256: await sha256(output),
      version: source.version,
      mode: releaseMode,
    }));
  } finally {
    if (attached) spawn("hdiutil", ["detach", mounted]);
    await rm(partial, { force: true });
    await rm(work, { recursive: true, force: true });
  }
};

main().catch((error) => {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
});
