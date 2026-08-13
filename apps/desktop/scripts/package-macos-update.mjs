import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { createReadStream } from "node:fs";
import {
  chmod,
  link,
  lstat,
  lutimes,
  mkdir,
  mkdtemp,
  open,
  readdir,
  realpath,
  rename,
  rm,
  unlink,
  utimes,
  writeFile,
} from "node:fs/promises";
import { basename, dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const APP_NAME = "WakeGPT.app";
const BUNDLE_ID = "com.wakegpt.desktop";
const EXECUTABLE_NAME = "wakegpt-desktop";
const TARGET = "darwin-universal";
const ARCHIVE_NAME = "WakeGPT_universal.app.tar.gz";
const SIGNATURE_NAME = `${ARCHIVE_NAME}.sig`;
const MANIFEST_NAME = "latest-darwin-universal.json";
const OUTPUT_NAMES = [ARCHIVE_NAME, SIGNATURE_NAME, MANIFEST_NAME];
const RELEASE_MODES = new Set([
  "adhoc-public-unnotarized",
  "developer-id-notarized",
]);
const SIGNING_ENVIRONMENT = [
  "TAURI_SIGNING_PRIVATE_KEY",
  "TAURI_SIGNING_PRIVATE_KEY_PATH",
  "TAURI_SIGNING_PRIVATE_KEY_PASSWORD",
];
const SEMVER = /^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/u;

const appRoot = fileURLToPath(new URL("..", import.meta.url));
const workspaceRoot = resolve(appRoot, "../..");
const manifestGenerator = fileURLToPath(new URL("./generate-update-manifest.mjs", import.meta.url));
const tauri = join(appRoot, "node_modules", ".bin", "tauri");

const parseArguments = (arguments_) => {
  const allowed = new Set([
    "--app",
    "--output-dir",
    "--version",
    "--repository",
    "--public-key",
    "--pub-date",
    "--release-mode",
    "--notes",
    "--source-date-epoch",
  ]);
  const values = new Map();
  for (let index = 0; index < arguments_.length; index += 1) {
    const argument = arguments_[index];
    if (!allowed.has(argument)) throw new Error(`Unknown argument: ${argument}`);
    if (values.has(argument)) throw new Error(`${argument} may be provided only once`);
    const value = arguments_[index + 1];
    if (!value || value.startsWith("--")) throw new Error(`${argument} requires a value`);
    values.set(argument, value);
    index += 1;
  }
  for (const argument of allowed) {
    if (!values.has(argument)) throw new Error(`${argument} is required`);
  }
  const input = {
    app: resolve(values.get("--app")),
    outputDirectory: resolve(values.get("--output-dir")),
    version: values.get("--version"),
    repository: values.get("--repository"),
    publicKey: resolve(values.get("--public-key")),
    pubDate: values.get("--pub-date"),
    releaseMode: values.get("--release-mode"),
    notes: resolve(values.get("--notes")),
    sourceDateEpoch: values.get("--source-date-epoch"),
  };
  if (basename(input.app) !== APP_NAME) throw new Error(`--app must identify ${APP_NAME}`);
  if (input.version.length > 64 || !SEMVER.test(input.version)) {
    throw new Error("--version must be an unprefixed SemVer value");
  }
  if (!RELEASE_MODES.has(input.releaseMode)) {
    throw new Error(`--release-mode must be one of: ${[...RELEASE_MODES].join(", ")}`);
  }
  if (!/^(?:0|[1-9]\d{0,11})$/u.test(input.sourceDateEpoch)) {
    throw new Error("--source-date-epoch must be a non-negative integer number of seconds");
  }
  const epoch = Number(input.sourceDateEpoch);
  if (!Number.isSafeInteger(epoch) || epoch > 253_402_300_799) {
    throw new Error("--source-date-epoch is outside the supported timestamp range");
  }
  input.epoch = epoch;
  const outputFromApp = relative(input.app, input.outputDirectory);
  const appFromOutput = relative(input.outputDirectory, input.app);
  if (
    outputFromApp === ""
    || (!outputFromApp.startsWith(`..${sep}`) && outputFromApp !== "..")
    || (!appFromOutput.startsWith(`..${sep}`) && appFromOutput !== "..")
  ) {
    throw new Error("--app and --output-dir must not overlap");
  }
  return input;
};

const cleanEnvironment = () => Object.fromEntries(
  Object.entries(process.env).filter(([name]) => !SIGNING_ENVIRONMENT.includes(name)),
);

const spawn = (command, arguments_, options = {}) => spawnSync(command, arguments_, {
  cwd: appRoot,
  encoding: "utf8",
  env: cleanEnvironment(),
  maxBuffer: 32 * 1024 * 1024,
  timeout: 5 * 60_000,
  ...options,
});

const run = (command, arguments_, options = {}) => {
  const { sensitive = false, ...spawnOptions } = options;
  const result = spawn(command, arguments_, spawnOptions);
  if (result.error || result.status !== 0) {
    if (sensitive) throw new Error("Tauri updater signing failed");
    const detail = `${result.stderr || result.stdout || result.error?.message || ""}`.trim();
    throw new Error(`${basename(command)} failed${detail ? `: ${detail}` : ""}`);
  }
  return `${result.stdout || ""}${result.stderr || ""}`;
};

const output = (command, arguments_, options) => run(command, arguments_, options).trim();

const validatePathType = async (path, expected, label) => {
  const metadata = await lstat(path);
  if (metadata.isSymbolicLink() || !metadata[expected]()) {
    throw new Error(`${label} must be a non-symlink ${expected === "isDirectory" ? "directory" : "file"}`);
  }
  return metadata;
};

const ensureMissing = async (path, label) => {
  try {
    await lstat(path);
    throw new Error(`${label} already exists`);
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
};

const plistValue = (info, key) => output("/usr/bin/plutil", [
  "-extract",
  key,
  "raw",
  "-o",
  "-",
  info,
]);

const verifyApp = async (app, input) => {
  await validatePathType(app, "isDirectory", APP_NAME);
  run("/usr/bin/codesign", ["--verify", "--deep", "--strict", "--verbose=2", app]);
  const signature = run("/usr/bin/codesign", ["-dv", "--verbose=4", app]);
  if (!/flags=.*\bruntime\b/mu.test(signature)) {
    throw new Error(`${APP_NAME} must enable the hardened runtime`);
  }
  if (input.releaseMode === "adhoc-public-unnotarized") {
    if (!/^Signature=adhoc$/mu.test(signature)) {
      throw new Error(`${APP_NAME} must use a complete ad-hoc signature for this release mode`);
    }
  } else {
    if (!/^Authority=Developer ID Application:/mu.test(signature)) {
      throw new Error(`${APP_NAME} must use a Developer ID Application signature`);
    }
    if (!/^TeamIdentifier=\S+/mu.test(signature) || !/^Timestamp=\S+/mu.test(signature)) {
      throw new Error(`${APP_NAME} must have a timestamped Developer ID signature`);
    }
    run("/usr/bin/xcrun", ["stapler", "validate", app]);
  }

  const info = join(app, "Contents", "Info.plist");
  await validatePathType(info, "isFile", "Info.plist");
  if (plistValue(info, "CFBundleIdentifier") !== BUNDLE_ID) {
    throw new Error(`${APP_NAME} has an unexpected bundle identifier`);
  }
  if (plistValue(info, "CFBundleExecutable") !== EXECUTABLE_NAME) {
    throw new Error(`${APP_NAME} has an unexpected executable name`);
  }
  if (
    plistValue(info, "CFBundleShortVersionString") !== input.version
    || plistValue(info, "CFBundleVersion") !== input.version
  ) {
    throw new Error(`${APP_NAME} version does not match --version`);
  }
  const executable = join(app, "Contents", "MacOS", EXECUTABLE_NAME);
  await validatePathType(executable, "isFile", "WakeGPT executable");
  const architectures = new Set(output("/usr/bin/lipo", ["-archs", executable]).split(/\s+/u));
  if (
    architectures.size !== 2
    || !architectures.has("arm64")
    || !architectures.has("x86_64")
  ) {
    throw new Error(`${APP_NAME} executable must contain exactly arm64 and x86_64 slices`);
  }
  return { executable };
};

const sha256 = async (path) => {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest("hex");
};

const syncFile = async (path) => {
  const handle = await open(path, "r");
  try {
    await handle.sync();
  } finally {
    await handle.close();
  }
};

const syncDirectory = async (path) => {
  const handle = await open(path, "r");
  try {
    await handle.sync();
  } finally {
    await handle.close();
  }
};

const isWithin = (parent, child) => {
  const path = relative(parent, child);
  return path === "" || (!path.startsWith(`..${sep}`) && path !== "..");
};

const signingEnvironment = async () => {
  const inline = `${process.env.TAURI_SIGNING_PRIVATE_KEY || ""}`.trim();
  const pathValue = `${process.env.TAURI_SIGNING_PRIVATE_KEY_PATH || ""}`.trim();
  if (Boolean(inline) === Boolean(pathValue)) {
    throw new Error("exactly one updater private key environment variable must be set");
  }
  const environment = cleanEnvironment();
  if (inline) {
    environment.TAURI_SIGNING_PRIVATE_KEY = inline;
  } else {
    const requested = resolve(pathValue);
    const metadata = await validatePathType(requested, "isFile", "updater private key");
    if ((metadata.mode & 0o777) !== 0o600) {
      throw new Error("updater private key file permissions must be 0600");
    }
    const [verifiedPath, verifiedWorkspace] = await Promise.all([
      realpath(requested),
      realpath(workspaceRoot),
    ]);
    if (isWithin(verifiedWorkspace, verifiedPath)) {
      throw new Error("updater private key must be stored outside the Git workspace");
    }
    environment.TAURI_SIGNING_PRIVATE_KEY_PATH = verifiedPath;
  }
  const password = `${process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD || ""}`;
  if (password) environment.TAURI_SIGNING_PRIVATE_KEY_PASSWORD = password;
  return environment;
};

const normalizeTimes = async (path, date) => {
  const metadata = await lstat(path);
  if (metadata.isDirectory() && !metadata.isSymbolicLink()) {
    const entries = await readdir(path);
    entries.sort((left, right) => Buffer.compare(Buffer.from(left), Buffer.from(right)));
    for (const entry of entries) await normalizeTimes(join(path, entry), date);
  }
  if (metadata.isSymbolicLink()) {
    await lutimes(path, date, date);
  } else {
    await utimes(path, date, date);
  }
};

const archiveEntries = async (root, path = APP_NAME) => {
  if (/\\|[\u0000-\u001f\u007f]/u.test(path)) {
    throw new Error(`${APP_NAME} contains a non-portable archive path`);
  }
  const absolute = join(root, path);
  const metadata = await lstat(absolute);
  const entries = [path];
  if (metadata.isDirectory() && !metadata.isSymbolicLink()) {
    const children = await readdir(absolute);
    children.sort((left, right) => Buffer.compare(Buffer.from(left), Buffer.from(right)));
    for (const child of children) entries.push(...await archiveEntries(root, join(path, child)));
  }
  return entries;
};

const createArchive = async (staging, archive, sourceDateEpoch, work) => {
  const entries = await archiveEntries(staging);
  entries.sort((left, right) => Buffer.compare(Buffer.from(left), Buffer.from(right)));
  const list = join(work, "archive-paths.bin");
  const uncompressed = join(work, "WakeGPT_universal.app.tar");
  await writeFile(list, Buffer.from(`${entries.join("\0")}\0`, "utf8"), { mode: 0o600 });
  run("/usr/bin/tar", [
    "--format=ustar",
    "--uid", "0",
    "--gid", "0",
    "--uname", "root",
    "--gname", "wheel",
    "--no-acls",
    "--no-xattrs",
    "--no-fflags",
    "--no-mac-metadata",
    "--no-recursion",
    "-cf", uncompressed,
    "-C", staging,
    "--null",
    "-T", list,
  ], {
    env: {
      ...cleanEnvironment(),
      COPYFILE_DISABLE: "1",
      LC_ALL: "C",
      SOURCE_DATE_EPOCH: `${sourceDateEpoch}`,
      TZ: "UTC",
    },
  });
  run("/usr/bin/gzip", ["-n", "-9", uncompressed], {
    env: {
      ...cleanEnvironment(),
      LC_ALL: "C",
      TZ: "UTC",
    },
  });
  await rename(`${uncompressed}.gz`, archive);
  await syncFile(archive);
};

const installOutputs = async (work, outputDirectory) => {
  const installed = [];
  try {
    for (const name of OUTPUT_NAMES) {
      const destination = join(outputDirectory, name);
      await link(join(work, name), destination);
      installed.push(destination);
      await syncFile(destination);
    }
    await syncDirectory(outputDirectory);
  } catch (error) {
    for (const path of installed.reverse()) await unlink(path).catch(() => {});
    await syncDirectory(outputDirectory).catch(() => {});
    throw error?.code === "EEXIST" ? new Error("release output collision detected") : error;
  }
};

const main = async () => {
  if (process.platform !== "darwin") {
    throw new Error("macOS updater packaging is supported only on macOS");
  }
  const input = parseArguments(process.argv.slice(2));
  await mkdir(input.outputDirectory, { recursive: true });
  await validatePathType(input.outputDirectory, "isDirectory", "updater output directory");
  for (const name of OUTPUT_NAMES) await ensureMissing(join(input.outputDirectory, name), name);
  const signerEnvironment = await signingEnvironment();
  const source = await verifyApp(input.app, input);
  const sourceExecutableHash = await sha256(source.executable);

  const work = await mkdtemp(join(input.outputDirectory, ".wakegpt-macos-update-"));
  await chmod(work, 0o700);
  const staging = join(work, "staging");
  const stagedApp = join(staging, APP_NAME);
  const extracted = join(work, "extracted");
  const archive = join(work, ARCHIVE_NAME);
  const signature = join(work, SIGNATURE_NAME);
  const manifest = join(work, MANIFEST_NAME);
  try {
    await mkdir(staging);
    await mkdir(extracted);
    run("/usr/bin/ditto", [input.app, stagedApp]);
    run("/usr/bin/diff", ["-rq", input.app, stagedApp]);
    const confirmedSource = await verifyApp(input.app, input);
    if (await sha256(confirmedSource.executable) !== sourceExecutableHash) {
      throw new Error(`${APP_NAME} changed while it was copied`);
    }

    const normalizedDate = new Date(input.epoch * 1000);
    await normalizeTimes(stagedApp, normalizedDate);
    const staged = await verifyApp(stagedApp, input);
    if (await sha256(staged.executable) !== sourceExecutableHash) {
      throw new Error("staged WakeGPT executable does not match the signed source app");
    }
    await createArchive(staging, archive, input.epoch, work);

    run(tauri, ["signer", "sign", archive], {
      env: signerEnvironment,
      sensitive: true,
    });
    await validatePathType(signature, "isFile", "update signature");
    await syncFile(signature);

    run(process.execPath, [
      manifestGenerator,
      "--version", input.version,
      "--repository", input.repository,
      "--target", TARGET,
      "--bundle", archive,
      "--signature", signature,
      "--public-key", input.publicKey,
      "--pub-date", input.pubDate,
      "--notes", input.notes,
      "--output", manifest,
    ]);
    await syncFile(manifest);

    run("/usr/bin/tar", ["-xzf", archive, "-C", extracted]);
    const extractedApp = join(extracted, APP_NAME);
    const extractedState = await verifyApp(extractedApp, input);
    run("/usr/bin/diff", ["-rq", stagedApp, extractedApp]);
    if (await sha256(extractedState.executable) !== sourceExecutableHash) {
      throw new Error("archived WakeGPT executable does not match the signed source app");
    }

    await installOutputs(work, input.outputDirectory);
    console.log(JSON.stringify({
      mode: input.releaseMode,
      version: input.version,
      target: TARGET,
      files: {
        archive: { name: ARCHIVE_NAME, sha256: await sha256(join(input.outputDirectory, ARCHIVE_NAME)) },
        signature: { name: SIGNATURE_NAME, sha256: await sha256(join(input.outputDirectory, SIGNATURE_NAME)) },
        manifest: { name: MANIFEST_NAME, sha256: await sha256(join(input.outputDirectory, MANIFEST_NAME)) },
      },
    }));
  } finally {
    await rm(work, { recursive: true, force: true });
  }
};

main().catch((error) => {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
});
