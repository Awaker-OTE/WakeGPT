import {
  createHash,
  createPublicKey,
  randomUUID,
  verify as verifyEd25519,
} from "node:crypto";
import { spawnSync } from "node:child_process";
import { constants } from "node:fs";
import { link, lstat, mkdir, open, readFile, rm } from "node:fs/promises";
import { basename, dirname, resolve } from "node:path";

const TARGET = "darwin-universal";
const BUNDLE_NAME = "WakeGPT_universal.app.tar.gz";
const OUTPUT_NAME = "latest-darwin-universal.json";
const MAX_BUNDLE_BYTES = 2 * 1024 * 1024 * 1024;
const MAX_SIGNATURE_BYTES = 8 * 1024;
const MAX_PUBLIC_KEY_BYTES = 4 * 1024;
const MAX_NOTES_BYTES = 64 * 1024;
const ED25519_SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");

const parseArguments = (arguments_) => {
  const allowed = new Set([
    "--version",
    "--repository",
    "--target",
    "--bundle",
    "--signature",
    "--public-key",
    "--pub-date",
    "--notes",
    "--output",
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
  return {
    version: values.get("--version"),
    repository: values.get("--repository"),
    target: values.get("--target"),
    bundle: resolve(values.get("--bundle")),
    signature: resolve(values.get("--signature")),
    publicKey: resolve(values.get("--public-key")),
    pubDate: values.get("--pub-date"),
    notes: resolve(values.get("--notes")),
    output: resolve(values.get("--output")),
  };
};

const semverPattern = /^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/u;
const ownerPattern = /^[A-Za-z0-9](?:[A-Za-z0-9-]{0,37}[A-Za-z0-9])?$/u;
const repositoryPattern = /^[A-Za-z0-9._-]{1,100}$/u;

const validateArguments = (input) => {
  if (input.target !== TARGET) throw new Error(`--target must be ${TARGET}`);
  if (input.version.length > 64 || !semverPattern.test(input.version)) {
    throw new Error("--version must be an unprefixed SemVer value");
  }
  const repositoryParts = input.repository.split("/");
  if (
    repositoryParts.length !== 2
    || !ownerPattern.test(repositoryParts[0])
    || !repositoryPattern.test(repositoryParts[1])
    || repositoryParts[1] === "."
    || repositoryParts[1] === ".."
    || repositoryParts[1].endsWith(".git")
  ) {
    throw new Error("--repository must be a portable GitHub owner/repository slug");
  }
  if (basename(input.bundle) !== BUNDLE_NAME) {
    throw new Error(`--bundle must be named ${BUNDLE_NAME}`);
  }
  if (basename(input.signature) !== `${BUNDLE_NAME}.sig`) {
    throw new Error(`--signature must be named ${BUNDLE_NAME}.sig`);
  }
  if (basename(input.output) !== OUTPUT_NAME) {
    throw new Error(`--output must be named ${OUTPUT_NAME}`);
  }
  if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/u.test(input.pubDate)) {
    throw new Error("--pub-date must be an RFC 3339 UTC timestamp without fractional seconds");
  }
  const parsedDate = new Date(input.pubDate);
  if (
    Number.isNaN(parsedDate.getTime())
    || parsedDate.toISOString().replace(".000Z", "Z") !== input.pubDate
  ) {
    throw new Error("--pub-date is not a real UTC timestamp");
  }
};

const validateProjectVersions = async (version) => {
  const [packageJson, tauriConfig, cargoToml] = await Promise.all([
    readFile(new URL("../package.json", import.meta.url), "utf8").then(JSON.parse),
    readFile(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8").then(JSON.parse),
    readFile(new URL("../src-tauri/Cargo.toml", import.meta.url), "utf8"),
  ]);
  const cargoVersion = cargoToml.match(/^\[package\][\s\S]*?^version\s*=\s*"([^"]+)"/mu)?.[1];
  const versions = [packageJson.version, tauriConfig.version, cargoVersion];
  if (versions.some((value) => value !== version)) {
    throw new Error("release version must match package.json, Cargo.toml, and tauri.conf.json");
  }
};

const sameIdentity = (first, second) => (
  first.dev === second.dev
  && first.ino === second.ino
  && first.size === second.size
  && first.mtimeMs === second.mtimeMs
);

const openStableFile = async (path, maximumBytes, label) => {
  const pathBefore = await lstat(path);
  if (pathBefore.isSymbolicLink() || !pathBefore.isFile()) {
    throw new Error(`${label} must be a regular non-symlink file`);
  }
  if (pathBefore.size === 0 || pathBefore.size > maximumBytes) {
    throw new Error(`${label} has an invalid size`);
  }
  const noFollow = constants.O_NOFOLLOW ?? 0;
  const handle = await open(path, constants.O_RDONLY | noFollow);
  const openedBefore = await handle.stat();
  if (!sameIdentity(pathBefore, openedBefore)) {
    await handle.close();
    throw new Error(`${label} changed before it was opened`);
  }
  return { handle, openedBefore, path, label };
};

const finishStableRead = async ({ handle, openedBefore, path, label }) => {
  const openedAfter = await handle.stat();
  const pathAfter = await lstat(path);
  if (!sameIdentity(openedBefore, openedAfter) || !sameIdentity(openedAfter, pathAfter)) {
    throw new Error(`${label} changed while it was read`);
  }
};

const readStableBytes = async (path, maximumBytes, label) => {
  const state = await openStableFile(path, maximumBytes, label);
  try {
    const bytes = Buffer.allocUnsafe(state.openedBefore.size);
    let position = 0;
    while (position < bytes.length) {
      const { bytesRead } = await state.handle.read(
        bytes,
        position,
        bytes.length - position,
        position,
      );
      if (bytesRead === 0) throw new Error(`${label} ended before its declared size`);
      position += bytesRead;
    }
    await finishStableRead(state);
    return bytes;
  } finally {
    await state.handle.close();
  }
};

const hashStableBundle = async (path) => {
  const state = await openStableFile(path, MAX_BUNDLE_BYTES, "update bundle");
  try {
    const sha256 = createHash("sha256");
    const blake2b = createHash("blake2b512");
    const buffer = Buffer.allocUnsafe(64 * 1024);
    let position = 0;
    while (true) {
      const { bytesRead } = await state.handle.read(buffer, 0, buffer.length, position);
      if (bytesRead === 0) break;
      const chunk = buffer.subarray(0, bytesRead);
      sha256.update(chunk);
      blake2b.update(chunk);
      position += bytesRead;
    }
    if (position !== state.openedBefore.size) {
      throw new Error("update bundle ended before its declared size");
    }
    await finishStableRead(state);
    return { sha256: sha256.digest("hex"), prehash: blake2b.digest() };
  } finally {
    await state.handle.close();
  }
};

const decodeUtf8 = (bytes, label) => {
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    throw new Error(`${label} must be valid UTF-8`);
  }
};

const decodeBase64 = (value, label) => {
  if (
    value.length === 0
    || value.length % 4 !== 0
    || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/u.test(value)
  ) {
    throw new Error(`${label} must use canonical base64`);
  }
  const decoded = Buffer.from(value, "base64");
  if (decoded.toString("base64") !== value) {
    throw new Error(`${label} must use canonical base64`);
  }
  return decoded;
};

const parsePublicKey = (encoded) => {
  const outer = encoded.trim();
  const lines = decodeUtf8(decodeBase64(outer, "public key"), "public key")
    .trimEnd()
    .split("\n");
  if (
    lines.length !== 2
    || !lines[0].startsWith("untrusted comment: minisign public key: ")
  ) {
    throw new Error("public key does not use the Tauri minisign format");
  }
  const binary = decodeBase64(lines[1], "minisign public key");
  if (binary.length !== 42 || binary[0] !== 0x45 || ![0x44, 0x64].includes(binary[1])) {
    throw new Error("public key has an unsupported minisign payload");
  }
  return {
    keyId: binary.subarray(2, 10),
    key: createPublicKey({
      key: Buffer.concat([ED25519_SPKI_PREFIX, binary.subarray(10)]),
      format: "der",
      type: "spki",
    }),
  };
};

const parseSignature = (encoded, bundleName) => {
  const outer = encoded.trim();
  const lines = decodeUtf8(decodeBase64(outer, "update signature"), "update signature")
    .trimEnd()
    .split("\n");
  if (
    lines.length !== 4
    || lines[0] !== "untrusted comment: signature from tauri secret key"
  ) {
    throw new Error("update signature does not use the Tauri minisign format");
  }
  const primary = decodeBase64(lines[1], "minisign primary signature");
  const global = decodeBase64(lines[3], "minisign global signature");
  if (
    primary.length !== 74
    || primary[0] !== 0x45
    || primary[1] !== 0x44
    || global.length !== 64
  ) {
    throw new Error("update signature must use the current prehashed Tauri format");
  }
  const trustedPrefix = "trusted comment: ";
  const trustedComment = lines[2].startsWith(trustedPrefix)
    ? lines[2].slice(trustedPrefix.length)
    : "";
  const expectedComment = new RegExp(
    `^timestamp:(?:0|[1-9]\\d*)\\tfile:${bundleName.replaceAll(".", "\\.")}$`,
    "u",
  );
  if (!expectedComment.test(trustedComment)) {
    throw new Error("update signature trusted comment does not match the update bundle");
  }
  return {
    encoded: outer,
    keyId: primary.subarray(2, 10),
    signature: primary.subarray(10),
    trustedComment,
    globalSignature: global,
  };
};

const verifySignature = (bundlePrehash, publicKey, signature) => {
  if (!publicKey.keyId.equals(signature.keyId)) {
    throw new Error("update signature key ID does not match the public key");
  }
  if (!verifyEd25519(null, bundlePrehash, publicKey.key, signature.signature)) {
    throw new Error("update bundle signature verification failed");
  }
  const globalMessage = Buffer.concat([
    signature.signature,
    Buffer.from(signature.trustedComment, "utf8"),
  ]);
  if (!verifyEd25519(null, globalMessage, publicKey.key, signature.globalSignature)) {
    throw new Error("update signature trusted comment verification failed");
  }
};

const verifyArchiveLayout = async (bundle, version) => {
  const before = await lstat(bundle);
  if (before.isSymbolicLink() || !before.isFile()) {
    throw new Error("update bundle must be a regular non-symlink file");
  }
  const tar = process.platform === "darwin" ? "/usr/bin/tar" : "tar";
  const result = spawnSync(tar, ["-tzf", bundle], {
    encoding: "utf8",
    maxBuffer: 8 * 1024 * 1024,
    timeout: 30_000,
  });
  if (result.error || result.status !== 0) {
    throw new Error("update bundle must be a readable gzip-compressed tar archive");
  }
  const entries = result.stdout.split("\n").filter(Boolean);
  if (entries.length === 0 || entries.length > 100_000) {
    throw new Error("update bundle has an invalid archive entry count");
  }
  for (const entry of entries) {
    const normalized = entry.replace(/^\.\//u, "");
    const segments = normalized.split("/").filter(Boolean);
    if (
      normalized.startsWith("/")
      || normalized.includes("\\")
      || /[\u0000-\u001f\u007f]/u.test(normalized)
      || segments.includes("..")
      || segments[0] !== "WakeGPT.app"
    ) {
      throw new Error("update bundle contains a path outside WakeGPT.app");
    }
  }
  if (
    !entries.some((entry) => entry.replace(/^\.\//u, "") === "WakeGPT.app/Contents/Info.plist")
    || !entries.some((entry) => entry.replace(/^\.\//u, "") === "WakeGPT.app/Contents/MacOS/wakegpt-desktop")
  ) {
      throw new Error("update bundle is missing the WakeGPT application identity files");
  }
  const infoResult = spawnSync(tar, [
    "-xOzf",
    bundle,
    "WakeGPT.app/Contents/Info.plist",
  ], {
    encoding: "utf8",
    maxBuffer: 1024 * 1024,
    timeout: 30_000,
  });
  if (infoResult.error || infoResult.status !== 0) {
    throw new Error("update bundle Info.plist could not be read without extraction");
  }
  const plistValue = (key) => infoResult.stdout.match(
    new RegExp(`<key>${key}</key>\\s*<string>([^<]+)</string>`, "u"),
  )?.[1];
  if (
    plistValue("CFBundleIdentifier") !== "com.wakegpt.desktop"
    || plistValue("CFBundleExecutable") !== "wakegpt-desktop"
    || plistValue("CFBundleShortVersionString") !== version
    || plistValue("CFBundleVersion") !== version
  ) {
    throw new Error("update bundle Info.plist does not match the WakeGPT release identity");
  }
  const after = await lstat(bundle);
  if (!sameIdentity(before, after)) {
    throw new Error("update bundle changed while its archive layout was inspected");
  }
};

const writeExclusive = async (output, contents) => {
  await mkdir(dirname(output), { recursive: true });
  try {
    await lstat(output);
    throw new Error(`${OUTPUT_NAME} already exists`);
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
  const temporary = `${output}.${randomUUID()}.partial`;
  try {
    const handle = await open(
      temporary,
      constants.O_CREAT | constants.O_EXCL | constants.O_WRONLY,
      0o644,
    );
    try {
      await handle.writeFile(contents, "utf8");
      await handle.sync();
    } finally {
      await handle.close();
    }
    await link(temporary, output);
    const directory = await open(dirname(output), constants.O_RDONLY);
    try {
      await directory.sync();
    } finally {
      await directory.close();
    }
  } finally {
    await rm(temporary, { force: true });
  }
};

const main = async () => {
  const input = parseArguments(process.argv.slice(2));
  validateArguments(input);
  await validateProjectVersions(input.version);
  const [bundle, signatureBytes, publicKeyBytes, notesBytes] = await Promise.all([
    hashStableBundle(input.bundle),
    readStableBytes(input.signature, MAX_SIGNATURE_BYTES, "update signature"),
    readStableBytes(input.publicKey, MAX_PUBLIC_KEY_BYTES, "updater public key"),
    readStableBytes(input.notes, MAX_NOTES_BYTES, "release notes"),
  ]);
  const signature = parseSignature(
    decodeUtf8(signatureBytes, "update signature"),
    basename(input.bundle),
  );
  const publicKey = parsePublicKey(decodeUtf8(publicKeyBytes, "updater public key"));
  verifySignature(bundle.prehash, publicKey, signature);
  await verifyArchiveLayout(input.bundle, input.version);
  const confirmedBundle = await hashStableBundle(input.bundle);
  if (
    confirmedBundle.sha256 !== bundle.sha256
    || !confirmedBundle.prehash.equals(bundle.prehash)
  ) {
    throw new Error("update bundle changed after signature verification");
  }
  const notes = decodeUtf8(notesBytes, "release notes").replace(/\r\n?/gu, "\n").trimEnd();
  if (!notes) throw new Error("release notes must not be empty");
  if (/\u0000|[\u0001-\u0008\u000b\u000c\u000e-\u001f\u007f]/u.test(notes)) {
    throw new Error("release notes contain unsupported control characters");
  }

  const url = `https://github.com/${input.repository}/releases/download/v${input.version}/${BUNDLE_NAME}`;
  const manifest = {
    version: input.version,
    notes,
    pub_date: input.pubDate,
    platforms: {
      [TARGET]: {
        signature: signature.encoded,
        url,
      },
    },
  };
  await writeExclusive(input.output, `${JSON.stringify(manifest, null, 2)}\n`);
  console.log(JSON.stringify({
    output: OUTPUT_NAME,
    version: input.version,
    target: TARGET,
    bundle: BUNDLE_NAME,
    bundleSha256: bundle.sha256,
  }));
};

main().catch((error) => {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
});
