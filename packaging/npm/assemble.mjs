#!/usr/bin/env node
// Builds a release's npm packages from its archives:
//
//   node packaging/npm/assemble.mjs <version> <dist> <out>
//
// <dist> holds the release archives (hidane-<version>-<target>.tar.gz, .zip for Windows).
// <out> receives one directory per package: the five @hidane/<platform> packages, each with its
// binary and the license files, and `hidane`, which lists them as optional dependencies. Publish
// the platform packages first.
//
//   node packaging/npm/assemble.mjs --placeholders <version> <out>
//
// writes the platform packages without binaries, to create them on npm once so that trusted
// publishing can be configured for them (docs/releasing.md).

import { execFileSync } from "node:child_process";
import { chmodSync, copyFileSync, cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "../..");

// npm platform → release target, `os`, `cpu`, description.
const PLATFORMS = {
  "darwin-arm64": ["aarch64-apple-darwin", "darwin", "arm64", "macOS arm64"],
  "darwin-x64": ["x86_64-apple-darwin", "darwin", "x64", "macOS x64"],
  "linux-arm64": ["aarch64-unknown-linux-musl", "linux", "arm64", "Linux arm64 (static, any libc)"],
  "linux-x64": ["x86_64-unknown-linux-musl", "linux", "x64", "Linux x64 (static, any libc)"],
  "win32-x64": ["x86_64-pc-windows-msvc", "win32", "x64", "Windows x64"],
};

const main = JSON.parse(readFileSync(join(here, "hidane/package.json"), "utf8"));

function writeJson(file, value) {
  writeFileSync(file, JSON.stringify(value, null, 2) + "\n");
}

function platformPackage(out, platform, version, archive) {
  const [target, os, cpu, label] = PLATFORMS[platform];
  const name = `@hidane/${platform}`;
  const dir = join(out, platform);
  mkdirSync(join(dir, "bin"), { recursive: true });
  const files = ["LICENSE-MIT", "LICENSE-APACHE"];
  if (archive) {
    const tmp = mkdtempSync(join(tmpdir(), "hidane-npm-"));
    if (archive.endsWith(".zip")) execFileSync("unzip", ["-q", archive, "-d", tmp]);
    else execFileSync("tar", ["-xzf", archive, "-C", tmp]);
    const unpacked = join(tmp, `hidane-${version}-${target}`);
    const exe = os === "win32" ? "hidane.exe" : "hidane";
    copyFileSync(join(unpacked, exe), join(dir, "bin", exe));
    chmodSync(join(dir, "bin", exe), 0o755);
    for (const file of ["LICENSE-MIT", "LICENSE-APACHE", "THIRD-PARTY-LICENSES.txt"]) {
      copyFileSync(join(unpacked, file), join(dir, file));
    }
    files.push("THIRD-PARTY-LICENSES.txt");
    rmSync(tmp, { recursive: true, force: true });
  } else {
    for (const file of ["LICENSE-MIT", "LICENSE-APACHE"]) copyFileSync(join(root, file), join(dir, file));
  }
  writeJson(join(dir, "package.json"), {
    name,
    version,
    description: archive
      ? `The hidane binary for ${label}. Install hidane, which picks the right one.`
      : `Reserved for the hidane binary for ${label}. Install hidane.`,
    homepage: main.homepage,
    repository: main.repository,
    license: main.license,
    os: [os],
    cpu: [cpu],
    files: archive ? ["bin", ...files] : files,
    preferUnplugged: true,
  });
  writeFileSync(
    join(dir, "README.md"),
    `# ${name}\n\nThe [hidane](https://github.com/hidane-dev/hidane) binary for ${label}. ` +
      "Install `hidane` instead: it depends on this package on this platform only.\n"
  );
  return name;
}

const args = process.argv.slice(2);
if (args[0] === "--placeholders") {
  const [, version, out] = args;
  if (!version || !out) throw new Error("usage: assemble.mjs --placeholders <version> <out>");
  for (const platform of Object.keys(PLATFORMS)) platformPackage(resolve(out), platform, version, null);
} else {
  const [version, dist, out] = args;
  if (!version || !dist || !out) throw new Error("usage: assemble.mjs <version> <dist> <out>");
  const optional = {};
  for (const platform of Object.keys(PLATFORMS)) {
    const target = PLATFORMS[platform][0];
    const archive = resolve(dist, `hidane-${version}-${target}${target.includes("windows") ? ".zip" : ".tar.gz"}`);
    optional[platformPackage(resolve(out), platform, version, archive)] = version;
  }
  const dir = resolve(out, "hidane");
  cpSync(join(here, "hidane"), dir, { recursive: true });
  for (const file of ["LICENSE-MIT", "LICENSE-APACHE"]) copyFileSync(join(root, file), join(dir, file));
  writeJson(join(dir, "package.json"), { ...main, version, optionalDependencies: optional });
}
