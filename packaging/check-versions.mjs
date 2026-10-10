#!/usr/bin/env node
// Checks that every place carrying hidane's version agrees with the workspace Cargo.toml: the
// `=` pins of hidane-core and hidane-proto, and the pub launcher's pubspec.yaml and constant.
// With a tag argument (`v<version>`), checks that too. Run by CI and the release workflow.

import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const read = (file) => readFileSync(join(root, file), "utf8");
const match = (file, pattern) => {
  const found = read(file).match(pattern);
  if (!found) throw new Error(`no version in ${file} (${pattern})`);
  return found[1];
};

const cargo = read("Cargo.toml");
const version = cargo.match(/\[workspace\.package\][^[]*?\nversion = "([^"]+)"/s)?.[1];
const found = {
  "Cargo.toml hidane-core pin": match("Cargo.toml", /hidane-core = \{[^}]*version = "=([^"]+)"/),
  "Cargo.toml hidane-proto pin": match("Cargo.toml", /hidane-proto = \{[^}]*version = "=([^"]+)"/),
  "packaging/pub/pubspec.yaml": match("packaging/pub/pubspec.yaml", /^version: (\S+)$/m),
  "packaging/pub/lib/hidane.dart": match("packaging/pub/lib/hidane.dart", /hidaneVersion = '([^']+)'/),
};
const tag = process.argv[2];
if (tag) found[`tag ${tag}`] = tag.replace(/^v/, "");

const wrong = Object.entries(found).filter(([, v]) => v !== version);
for (const [where, v] of wrong) console.error(`${where}: ${v}, expected ${version}`);
if (!version || wrong.length) process.exit(1);
console.log(`hidane ${version}: ${Object.keys(found).length + 1} places agree`);
