// The steps of tools/oracle/sdk_rules_unit_testing.mjs in a browser, where the web SDK speaks
// WebChannel (results/browser-rules-*.json): the contexts' mock tokens reach the emulator in the
// handshake's `headers=` field, a denied write ends the Write stream, a denied listener's target is
// removed with a cause, and transactions go over REST.
//
// Bundle it from a directory outside the repository where
// `npm i firebase @firebase/rules-unit-testing esbuild` was run, with sdk_rules_unit_testing.mjs
// copied next to a `webchannel/` directory holding this file (esbuild resolves imports from the
// file's own directory):
//   npx esbuild webchannel/rules_page.js --bundle --format=iife --define:process.env={} --outfile=bundle.js
// (rules-unit-testing reads `process.env` for emulators it is not given). Serve `bundle.js` from a
// page on another origin (CORS is part of what is tested) and call `rulesUnitTesting({host, port})`
// from the page, with Playwright or the browser console, once per freshly started emulator.
import { rulesUnitTesting } from '../sdk_rules_unit_testing.mjs';

window.rulesUnitTesting = rulesUnitTesting;
