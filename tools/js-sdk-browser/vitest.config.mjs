// Copied into firebase-js-sdk's packages/firestore/ as vitest.hidane.config.mjs by
// tools/js-sdk-browser/run.sh: the package's own configuration, with cleanup.ts as one more
// setup file of the browser tests.
import path from 'path';
import { fileURLToPath } from 'url';

import config from './vitest.config.mjs';

const cleanup = path.resolve(path.dirname(fileURLToPath(import.meta.url)), 'test/hidane_cleanup.ts');

config.test.projects = config.test.projects.map(project =>
  project.test?.name === 'browser'
    ? { ...project, test: { ...project.test, setupFiles: [...(project.test.setupFiles ?? []), cleanup] } }
    : project
);

export default config;
