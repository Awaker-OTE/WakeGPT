import { spawnSync } from "node:child_process";
import { constants } from "node:fs";
import { access, readFile, readdir, realpath, stat } from "node:fs/promises";
import { dirname, join } from "node:path";
import { homedir } from "node:os";
import { fileURLToPath } from "node:url";

const TARGETS = ["aarch64-apple-darwin", "x86_64-apple-darwin"];
const appRoot = fileURLToPath(new URL("..", import.meta.url));
const workspaceRoot = fileURLToPath(new URL("../../..", import.meta.url));
const toolchainFile = join(workspaceRoot, "rust-toolchain.toml");
const tauri = join(appRoot, "node_modules", ".bin", "tauri");

const parseArguments = (arguments_) => {
  if (arguments_.length === 0) return { checkOnly: false };
  if (arguments_.length === 1 && arguments_[0] === "--check-only") return { checkOnly: true };
  throw new Error("Only --check-only is supported");
};

const run = (command, arguments_, options = {}) => {
  const result = spawnSync(command, arguments_, {
    cwd: appRoot,
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
    timeout: 60_000,
    ...options,
  });
  if (result.error || result.status !== 0) {
    const detail = `${result.stderr || result.stdout || result.error?.message || ""}`.trim();
    throw new Error(`${command.split("/").at(-1)} failed${detail ? `: ${detail}` : ""}`);
  }
  return `${result.stdout || ""}${result.stderr || ""}`.trim();
};

const regularExecutable = async (path, label) => {
  const canonical = await realpath(path);
  const metadata = await stat(canonical);
  if (!metadata.isFile()) throw new Error(`${label} must resolve to a regular file`);
  await access(canonical, constants.X_OK);
  return canonical;
};

const findRustup = async () => {
  const candidates = [
    join(homedir(), ".cargo", "bin", "rustup"),
    "/opt/homebrew/bin/rustup",
    "/usr/local/bin/rustup",
  ];
  const discovered = spawnSync("/usr/bin/which", ["rustup"], {
    encoding: "utf8",
    timeout: 10_000,
  });
  if (discovered.status === 0 && discovered.stdout.trim()) {
    candidates.push(discovered.stdout.trim());
  }
  for (const candidate of [...new Set(candidates)]) {
    try {
      return await regularExecutable(candidate, "rustup");
    } catch (error) {
      if (!["ENOENT", "EACCES"].includes(error?.code)) throw error;
    }
  }
  throw new Error("rustup is required for the macOS Universal release build");
};

const configuredChannel = async () => {
  const source = await readFile(toolchainFile, "utf8");
  const channel = source.match(/^channel\s*=\s*"([^"]+)"\s*$/mu)?.[1];
  if (!channel || !/^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)$/u.test(channel)) {
    throw new Error("rust-toolchain.toml must pin an exact stable Rust version");
  }
  return channel;
};

const rustupTool = async (rustup, channel, name) => {
  const path = run(rustup, ["which", "--toolchain", channel, name]);
  return regularExecutable(path, `rustup ${name}`);
};

const validateTargetLibraries = async (rustc, target) => {
  const directory = await realpath(run(rustc, ["--target", target, "--print", "target-libdir"]));
  const metadata = await stat(directory);
  if (!metadata.isDirectory()) throw new Error(`${target} target libdir is not a directory`);
  const names = await readdir(directory);
  if (
    !names.some((name) => /^libcore-.+\.rlib$/u.test(name))
    || !names.some((name) => /^libstd-.+\.rlib$/u.test(name))
  ) {
    throw new Error(`${target} rust-std is incomplete`);
  }
};

const resolveToolchain = async () => {
  const [rustup, channel] = await Promise.all([findRustup(), configuredChannel()]);
  const [cargo, rustc, rustdoc] = await Promise.all([
    rustupTool(rustup, channel, "cargo"),
    rustupTool(rustup, channel, "rustc"),
    rustupTool(rustup, channel, "rustdoc"),
  ]);
  if (new Set([dirname(cargo), dirname(rustc), dirname(rustdoc)]).size !== 1) {
    throw new Error("rustup cargo, rustc, and rustdoc must come from one toolchain directory");
  }

  const rustcDetails = run(rustc, ["-vV"]);
  const cargoDetails = run(cargo, ["-vV"]);
  const rustcRelease = rustcDetails.match(/^release:\s*(\S+)$/mu)?.[1];
  const cargoRelease = cargoDetails.match(/^release:\s*(\S+)$/mu)?.[1];
  const host = rustcDetails.match(/^host:\s*(\S+)$/mu)?.[1];
  if (rustcRelease !== channel || cargoRelease !== channel || !host?.endsWith("-apple-darwin")) {
    throw new Error("rustup cargo/rustc versions or host do not match rust-toolchain.toml");
  }

  const installed = new Set(
    run(rustup, ["target", "list", "--installed", "--toolchain", channel])
      .split("\n")
      .map((value) => value.trim())
      .filter(Boolean),
  );
  for (const target of TARGETS) {
    if (!installed.has(target)) {
      throw new Error(`${target} is not installed for Rust ${channel}`);
    }
    await validateTargetLibraries(rustc, target);
  }
  return { cargo, channel, host, rustc, rustdoc, toolDirectory: dirname(cargo) };
};

const main = async () => {
  if (process.platform !== "darwin") {
    throw new Error("macOS Universal release builds are supported only on macOS");
  }
  const { checkOnly } = parseArguments(process.argv.slice(2));
  const toolchain = await resolveToolchain();
  if (checkOnly) {
    console.log(JSON.stringify({
      source: "rustup",
      toolchain: toolchain.channel,
      host: toolchain.host,
      targets: TARGETS,
    }));
    return;
  }

  const environment = {
    ...process.env,
    CARGO: toolchain.cargo,
    RUSTC: toolchain.rustc,
    RUSTDOC: toolchain.rustdoc,
    RUSTUP_TOOLCHAIN: toolchain.channel,
    PATH: `${toolchain.toolDirectory}:${process.env.PATH || ""}`,
  };
  const result = spawnSync(tauri, ["build", "--bundles", "app", "--target", "universal-apple-darwin"], {
    cwd: appRoot,
    env: environment,
    stdio: "inherit",
  });
  if (result.error || result.status !== 0) {
    throw new Error("Tauri macOS Universal release build failed");
  }
};

main().catch((error) => {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
});
