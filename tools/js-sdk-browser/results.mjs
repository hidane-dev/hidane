// Turns the reports of Vitest's JSON reporter (one per test file run) into the results kept in
// results/: each test's ID (its file, relative to packages/firestore, then its describe blocks
// and its title, joined with " > ") and its outcome, sorted, without times, paths or messages.
//
//   node results.mjs <firebase-js-sdk commit> <emulator> <vitest-report.json>... > results.json

import { readFileSync } from 'node:fs';

const [commit, emulator, ...reports] = process.argv.slice(2);
if (!commit || !emulator || reports.length === 0) {
  console.error('usage: node results.mjs <firebase-js-sdk commit> <emulator> <vitest-report.json>...');
  process.exit(2);
}

const OUTCOMES = { passed: 'passed', failed: 'failed', skipped: 'skipped', pending: 'skipped', todo: 'skipped' };
const ROOT = '/packages/firestore/';
// A few titles print a `new Date()` (`set/update should reject: Tue Oct 13 2026 03:04:54
// GMT+0000 (Coordinated Universal Time)`); the ID has `<date>` in its place.
const DATE = /[A-Z][a-z]{2} [A-Z][a-z]{2} \d{2} \d{4} \d{2}:\d{2}:\d{2} GMT[+-]\d{4}( \([^)]*\))?/g;

const tests = new Map();
const files = reports.flatMap(report => JSON.parse(readFileSync(report, 'utf8')).testResults);
for (const file of files) {
  const at = file.name.lastIndexOf(ROOT);
  const path = at < 0 ? file.name : file.name.slice(at + ROOT.length);
  if (file.status === 'failed' && !file.assertionResults.some(t => t.status === 'failed')) {
    // The file failed outside its tests: an import, a hook or an unhandled error.
    tests.set(`${path} > (file)`, 'failed');
  }
  for (const test of file.assertionResults) {
    const outcome = OUTCOMES[test.status];
    if (!outcome) throw new Error(`unknown status ${test.status} for ${test.fullName}`);
    const base = [path, ...test.ancestorTitles, test.title].join(' > ').replace(DATE, '<date>');
    // A few suites declare two tests with the same name; number the repeats.
    let id = base;
    for (let n = 2; tests.has(id); n++) id = `${base} (${n})`;
    tests.set(id, outcome);
  }
}

const sorted = [...tests.keys()].sort();
const counts = { passed: 0, failed: 0, skipped: 0 };
for (const id of sorted) counts[tests.get(id)]++;
const result = {
  'firebase-js-sdk': commit,
  emulator,
  counts,
  tests: Object.fromEntries(sorted.map(id => [id, tests.get(id)]))
};
process.stdout.write(JSON.stringify(result, null, 2) + '\n');
