// Turns the report of Mocha's JSON reporter into the results kept in results/: each test's ID
// (its file, relative to build/, then its full title, joined with " > ") and its outcome,
// sorted, without times, paths or messages.
//
//   node results.mjs <google-cloud-node commit> <emulator> <mocha-report.json> [grpc|rest] > results.json

import { readFileSync } from 'node:fs';

const [commit, emulator, reportPath, transport = 'grpc'] = process.argv.slice(2);
if (!commit || !emulator || !reportPath || !['grpc', 'rest'].includes(transport)) {
  console.error('usage: node results.mjs <google-cloud-node commit> <emulator> <mocha-report.json> [grpc|rest]');
  process.exit(2);
}

const report = JSON.parse(readFileSync(reportPath, 'utf8'));
const ROOT = '/build/';
const id = test => {
  const at = test.file ? test.file.lastIndexOf(ROOT) : -1;
  const file = at < 0 ? '(unknown file)' : test.file.slice(at + ROOT.length);
  return `${file} > ${test.fullTitle}`;
};

// A failed hook is a failure without a test of its own; it is listed under its own title.
const outcomes = new Map();
const seen = new Map();
const add = (test, outcome) => {
  const base = id(test);
  // A few suites declare two tests with the same title; number the repeats in report order.
  const n = (seen.get(base) ?? 0) + 1;
  seen.set(base, n);
  outcomes.set(n === 1 ? base : `${base} (${n})`, outcome);
};
// How many tests of each file and title failed or were skipped: titles repeat, so a test takes
// one of them at a time.
const key = test => `${test.file} ${test.fullTitle}`;
const tally = tests => {
  const counts = new Map();
  for (const test of tests) counts.set(key(test), (counts.get(key(test)) ?? 0) + 1);
  return counts;
};
const take = (counts, test) => {
  const left = counts.get(key(test)) ?? 0;
  if (left === 0) return false;
  counts.set(key(test), left - 1);
  return true;
};
const failed = tally(report.failures);
const pending = tally(report.pending);
for (const test of report.tests) {
  add(test, take(failed, test) ? 'failed' : take(pending, test) ? 'skipped' : 'passed');
}
for (const test of report.failures) {
  if (take(failed, test)) add(test, 'failed');
}

const sorted = [...outcomes.keys()].sort();
const counts = { passed: 0, failed: 0, skipped: 0 };
for (const key of sorted) counts[outcomes.get(key)]++;
const result = {
  'google-cloud-node': commit,
  emulator,
  transport,
  counts,
  tests: Object.fromEntries(sorted.map(key => [key, outcomes.get(key)]))
};
process.stdout.write(JSON.stringify(result, null, 2) + '\n');
