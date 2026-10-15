// Compares a run of nodejs-firestore's system tests against hidane with the run against
// the official emulator (both as written by results.mjs), and fails when they disagree in a way
// expected-failures.txt does not list:
//
// - a test that passed on the official emulator and did not pass on hidane, unless listed;
// - a listed test that passed on hidane (the list must stay honest: take it out);
// - a listed test the official run does not pass (the list is for differences only);
// - runs of different google-cloud-node commits, or a test of one run that the other lacks.
//
//   node compare.mjs <official.json> <hidane.json> <expected-failures.txt>

import { readFileSync } from 'node:fs';

const [officialPath, hidanePath, listPath] = process.argv.slice(2);
if (!officialPath || !hidanePath || !listPath) {
  console.error('usage: node compare.mjs <official.json> <hidane.json> <expected-failures.txt>');
  process.exit(2);
}

const official = JSON.parse(readFileSync(officialPath, 'utf8'));
const hidane = JSON.parse(readFileSync(hidanePath, 'utf8'));

// Each entry is a test ID on a line of its own, after a `#` comment with the reason.
const expected = new Set(
  readFileSync(listPath, 'utf8')
    .split('\n')
    .map(line => line.trim())
    .filter(line => line && !line.startsWith('#'))
);

const problems = [];
if (official['google-cloud-node'] !== hidane['google-cloud-node']) {
  problems.push(
    `the runs are of different google-cloud-node commits: ${official['google-cloud-node']} (official) and ` +
      `${hidane['google-cloud-node']} (hidane); record the official run again at the pinned commit`
  );
}

const unexpected = [];
const nowPassing = [];
const notDifferences = [];
for (const [id, outcome] of Object.entries(official.tests)) {
  const ours = hidane.tests[id];
  if (ours === undefined) {
    problems.push(`missing from the hidane run: ${id}`);
  } else if (outcome === 'passed' && ours !== 'passed' && !expected.has(id)) {
    unexpected.push(`${id} (${ours})`);
  }
}
for (const id of Object.keys(hidane.tests)) {
  if (!(id in official.tests)) problems.push(`missing from the official run: ${id}`);
}
for (const id of expected) {
  if (!(id in official.tests)) {
    problems.push(`listed in ${listPath} but not run: ${id}`);
  } else if (official.tests[id] !== 'passed') {
    notDifferences.push(`${id} (${official.tests[id]} on the official emulator)`);
  } else if (hidane.tests[id] === 'passed') {
    nowPassing.push(id);
  }
}

const sections = [
  ['Passed on the official emulator, not on hidane, and not listed as expected', unexpected],
  ['Listed as expected failures, but passed on hidane (take them out of the list)', nowPassing],
  ['Listed as expected failures, but not passed on the official emulator', notDifferences],
  ['Other problems', problems]
];

const line = (name, run) =>
  `${name}: ${run.counts.passed} passed, ${run.counts.failed} failed, ${run.counts.skipped} skipped`;
console.log(line('official emulator', official));
console.log(line('hidane', hidane));
console.log(`expected failures listed: ${expected.size}`);

let failed = false;
for (const [title, entries] of sections) {
  if (entries.length === 0) continue;
  failed = true;
  console.log(`\n${title} (${entries.length}):`);
  for (const entry of entries) console.log(`  ${entry}`);
}
if (failed) process.exit(1);
console.log('\nhidane passes every test the official emulator passes, apart from the listed ones.');
