#!/usr/bin/env node
// Runs hidane's binary for this platform. npm installs it with the one optional dependency
// (`@hidane/<platform>`) whose `os` and `cpu` match; packaging/npm/assemble.mjs builds them.
"use strict";

const { spawn } = require("node:child_process");

const PACKAGES = {
  "darwin arm64": "@hidane/darwin-arm64",
  "darwin x64": "@hidane/darwin-x64",
  "linux arm64": "@hidane/linux-arm64",
  "linux x64": "@hidane/linux-x64",
  "win32 x64": "@hidane/win32-x64",
};

function fail(message) {
  process.stderr.write(`hidane: ${message}\n`);
  process.exit(1);
}

function binary() {
  const platform = `${process.platform} ${process.arch}`;
  const name = PACKAGES[platform];
  if (!name) {
    fail(
      `there is no binary for ${platform}. Supported: ${Object.keys(PACKAGES).join(", ")}. ` +
        "Other platforms can build from source: cargo install hidane"
    );
  }
  const file = `${name}/bin/hidane${process.platform === "win32" ? ".exe" : ""}`;
  try {
    return require.resolve(file);
  } catch {
    fail(
      `${name}, which holds the binary for ${platform}, is not installed. ` +
        "npm leaves optional dependencies out with --omit=optional (or --no-optional); reinstall without it."
    );
  }
}

const bin = binary();
const child = spawn(bin, process.argv.slice(2), { stdio: "inherit" });

// Stay alive until hidane exits, and pass on signals sent to this process alone (a supervisor's
// SIGINT or SIGTERM). From a terminal hidane receives Ctrl-C itself; a second SIGINT does no harm.
// On Windows, Ctrl-C reaches every console process and killing would not be graceful.
for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.on(signal, () => {
    if (process.platform !== "win32") child.kill(signal);
  });
}

child.on("error", (err) => fail(`could not run ${bin}: ${err.message}`));
child.on("exit", (code, signal) => {
  if (signal) {
    process.removeAllListeners(signal);
    process.kill(process.pid, signal);
    return;
  }
  process.exit(code ?? 1);
});
