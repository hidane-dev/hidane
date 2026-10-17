// Runs one section of steps through @google-cloud/datastore (the Node.js client library of
// Datastore) and prints a transcript, so the transcripts of two emulators in Datastore mode can be
// diffed (#81). DATASTORE_EMULATOR_HOST selects the emulator, started with `--database-mode
// datastore-mode`; start it fresh for each section. The client speaks gRPC: its REST fallback
// (`fallback: 'rest'`) looks for credentials even against an emulator.
//
// Usage: node sdk_datastore_node.mjs entities|queries|transactions > transcript.json
//
// The sections and their step names are those of tools/oracle/sdk_datastore_python/.
import { Datastore, PropertyFilter, and, or } from '@google-cloud/datastore';

const section = process.argv[2];
if (!['entities', 'queries', 'transactions'].includes(section)) {
  console.error('usage: node sdk_datastore_node.mjs entities|queries|transactions');
  process.exit(2);
}
const projectId = `demo-datastore-node-${section}`;
const ds = new Datastore({ projectId });
const out = [];
const mask = (s) => String(s).replaceAll(projectId, '{project}').replace(/\d{10,}/g, '<n>');

async function step(name, fn) {
  try {
    out.push({ name, ok: await fn() });
  } catch (e) {
    out.push({ name, error: typeof e.code === 'number'
      ? { code: e.code, message: mask(e.details ?? e.message) }
      : { client: mask(e.message) } });
  }
}

function keyOf(key) {
  const path = key.path.map((p) => (typeof p === 'object' ? String(p) : p));
  return key.namespace ? { path, namespace: key.namespace } : { path };
}

function norm(v) {
  if (v === null || v === undefined || typeof v === 'string' || typeof v === 'boolean') return v ?? null;
  if (typeof v === 'number') {
    if (Number.isNaN(v)) return 'NaN';
    if (!Number.isFinite(v)) return v > 0 ? 'Infinity' : '-Infinity';
    return v;
  }
  if (v instanceof Date) return v.toISOString();
  if (Buffer.isBuffer(v)) return { bytes: v.toString('base64') };
  if (ds.isKey(v)) return { key: keyOf(v) };
  if (ds.isGeoPoint(v)) return { geopoint: [v.value.latitude, v.value.longitude] };
  if (ds.isInt(v) || ds.isDouble(v)) return norm(Number(v.value));
  if (Array.isArray(v)) return v.map(norm);
  if (typeof v === 'object') return entity(v);
  return { unknown: String(v) };
}

function entity(e) {
  if (!e) return null;
  const properties = {};
  for (const name of Object.keys(e).sort()) properties[name] = norm(e[name]);
  const key = e[Datastore.KEY];
  return key ? { properties, key: keyOf(key) } : { properties };
}

const keys = (entities) => entities.map((e) => keyOf(e[Datastore.KEY]));
const get = async (key) => entity((await ds.get(key))[0]);

// --- entities -------------------------------------------------------------------------------

async function entities() {
  const task = ds.key(['Task', 't1']);
  await step('put every type', async () => {
    await ds.save({
      key: task,
      excludeFromIndexes: ['long'],
      data: {
        text: 'a', count: 3, ratio: ds.double(0.5), done: false, nothing: null,
        at: new Date('2020-01-02T03:04:05.678Z'), raw: Buffer.from([0, 1]),
        place: ds.geoPoint({ latitude: 35, longitude: 139 }), owner: ds.key(['User', 'u1']),
        tags: ['x', 'y'], empty: [], nested: { x: 1, y: 'z' }, long: 'b'.repeat(2000),
        nan: ds.double(NaN), inf: ds.double(Infinity), ninf: ds.double(-Infinity),
      },
    });
    return 'ok';
  });
  await step('get it back', () => get(task));
  await step('get a missing entity', () => get(ds.key(['Task', 'nobody'])));
  await step('get several, one missing', async () => keys((await ds.get([task, ds.key(['Task', 'nobody'])]))[0]));
  await step('overwrite it', async () => { await ds.save({ key: task, data: { text: 'b' } }); return get(task); });
  await step('put an incomplete key', async () => {
    const key = ds.key(['Task']);
    await ds.save({ key, data: { text: 'generated' } });
    return { id: key.id, read: await get(key) };
  });
  await step('allocate three IDs', async () => (await ds.allocateIds(ds.key(['Task']), 3))[0].map((k) => k.id));
  await step('allocate an ID under a parent', async () => (await ds.allocateIds(ds.key(['List', 'l1', 'Task']), 1))[0].map(keyOf));
  const child = ds.key(['List', 'l1', 'Task', 'c1']);
  await step('put a child', async () => { await ds.save({ key: child, data: { text: 'child' } }); return get(child); });
  await step('get a child\'s parent, never written', () => get(child.parent));
  await step('put in a namespace', async () => {
    const key = ds.key({ namespace: 'ns1', path: ['Task', 't1'] });
    await ds.save({ key, data: { text: 'ns' } });
    return get(key);
  });
  await step('the default namespace keeps its own', async () => (await ds.get(task))[0].text);
  const many = [1, 2, 3].map((i) => ds.key(['Many', ds.int(i)]));
  await step('put several', async () => {
    await ds.save(many.map((key, i) => ({ key, data: { i: i + 1 } })));
    return keys((await ds.get(many))[0]);
  });
  await step('delete several', async () => { await ds.delete(many.slice(0, 2)); return keys((await ds.get(many))[0]); });
  await step('delete a missing entity', async () => { await ds.delete(ds.key(['Task', 'nobody'])); return 'ok'; });
  await step('delete it', async () => { await ds.delete(task); return get(task); });
  await step('insert an existing entity', async () => {
    const key = ds.key(['Task', 'dup']);
    await ds.save({ key, data: { a: 1 } });
    await ds.insert({ key, data: { a: 2 } });
    return 'ok';
  });
  await step('update a missing entity', async () => { await ds.update({ key: ds.key(['Task', 'nobody']), data: { a: 1 } }); return 'ok'; });
  await step('an indexed string of 1501 bytes', async () => { await ds.save({ key: ds.key(['Task', 'long']), data: { text: 'a'.repeat(1501) } }); return 'ok'; });
  await step('a property named __x__', async () => { await ds.save({ key: ds.key(['Task', 'r']), data: { __x__: 1 } }); return 'ok'; });
  await step('an array in an array', async () => { await ds.save({ key: ds.key(['Task', 'r']), data: { a: [[1]] } }); return 'ok'; });
  await step('a kind __x__', async () => { await ds.save({ key: ds.key(['__x__', 'a']), data: { a: 1 } }); return 'ok'; });
}

// --- queries --------------------------------------------------------------------------------

async function queries() {
  const people = [
    ['p1', { a: 1, b: 'x', tags: ['red', 'blue'] }], ['p2', { a: 2, b: 'y', tags: ['red'] }],
    ['p3', { a: 2, b: 'x', tags: [] }], ['p4', { a: 3, b: 'z', tags: ['blue', 'green'] }],
    ['p5', { a: 3, b: 'y' }], ['p6', { a: -1, b: 'x', tags: ['green'] }], ['p7', { b: 'w' }],
    ['p8', { a: ds.double(4.5), b: 'x' }],
  ];
  await ds.save(people.map(([name, data]) => ({ key: ds.key(['P', name]), data })));
  const root = ds.key(['F', 'root']);
  await ds.save([
    { key: root, data: { n: 0 } }, { key: ds.key(['F', 'root', 'C', 'c1']), data: { n: 1 } },
    { key: ds.key(['F', 'root', 'C', 'c2']), data: { n: 2 } }, { key: ds.key(['C', 'c3']), data: { n: 3 } },
  ]);
  await ds.save({ key: ds.key({ namespace: 'ns1', path: ['P', 'n1'] }), data: { a: 1, b: 'x' } });
  const P = () => ds.createQuery('P');
  const run = async (q) => keys((await ds.runQuery(q))[0]);
  const rows = async (q) => (await ds.runQuery(q))[0].map(entity);

  await step('kind P', () => run(P()));
  for (const [op, value] of [['=', 2], ['<', 2], ['<=', 2], ['>', 2], ['>=', 2], ['!=', 2], ['=', 'x'],
    ['IN', [1, 3]], ['NOT_IN', [2, 3]], ['=', null], ['=', ds.double(4.5)]]) {
    await step(`a ${op} ${JSON.stringify(norm(value))}`, () => run(P().filter(new PropertyFilter('a', op, value))));
  }
  await step('tags = red', () => run(P().filter(new PropertyFilter('tags', '=', 'red'))));
  await step('a = 2 and b = x', () => run(P().filter(and([new PropertyFilter('a', '=', 2), new PropertyFilter('b', '=', 'x')]))));
  await step('a = 1 or b = y', () => run(P().filter(or([new PropertyFilter('a', '=', 1), new PropertyFilter('b', '=', 'y')]))));
  await step('a > 1 and b > w', () => run(P().filter(new PropertyFilter('a', '>', 1)).filter(new PropertyFilter('b', '>', 'w'))));
  await step('order by a', () => run(P().order('a')));
  await step('order by -a, b', () => run(P().order('a', { descending: true }).order('b')));
  await step('order by b, -__key__', () => run(P().order('b').order('__key__', { descending: true })));
  await step('__key__ > p4', () => run(P().filter(new PropertyFilter('__key__', '>', ds.key(['P', 'p4'])))));
  await step('keys only', () => run(P().select('__key__')));
  await step('project a, b', () => rows(P().select(['a', 'b'])));
  await step('distinct on b', () => rows(P().select('b').groupBy('b')));
  await step('limit 2', () => run(P().limit(2)));
  await step('offset 6', () => run(P().offset(6)));
  await step('limit 2, offset 3', () => run(P().limit(2).offset(3)));
  await step('pages of 3 by cursor', async () => {
    const pages = [];
    let cursor;
    for (let i = 0; i < 5; i++) {
      const q = P().order('b').limit(3);
      if (cursor) q.start(cursor);
      const [entities, info] = await ds.runQuery(q);
      if (entities.length === 0) break;
      pages.push(keys(entities));
      if (info.moreResults === Datastore.NO_MORE_RESULTS) break;
      cursor = info.endCursor;
    }
    return pages;
  });
  await step('children of F root', () => run(ds.createQuery('C').hasAncestor(root)));
  await step('everything under F root', () => run(ds.createQuery().hasAncestor(root)));
  await step('kind P in ns1', () => run(ds.createQuery('ns1', 'P')));
  await step('kinds', () => run(ds.createQuery('__kind__').select('__key__')));
  await step('namespaces', () => run(ds.createQuery('__namespace__').select('__key__')));
  await step('a kind with no entities', () => run(ds.createQuery('Nothing')));
  const aggregate = async (aq) => {
    const [[result]] = await ds.runAggregationQuery(aq);
    return Object.fromEntries(Object.keys(result).sort().map((k) => [k, norm(result[k])]));
  };
  await step('count P', () => aggregate(ds.createAggregationQuery(P()).count('n')));
  await step('count b = x', () => aggregate(ds.createAggregationQuery(P().filter(new PropertyFilter('b', '=', 'x'))).count('n')));
  await step('sum and avg of a', () => aggregate(ds.createAggregationQuery(P()).sum('a', 's').average('a', 'm')));
  await step('IN with 31 values', () => run(P().filter(new PropertyFilter('a', 'IN', [...Array(31).keys()]))));
  await step('order twice by a', () => run(P().order('a').order('a', { descending: true })));
  await step('project a, filter a = 1', () => rows(P().select('a').filter(new PropertyFilter('a', '=', 1))));
}

// --- transactions ---------------------------------------------------------------------------

async function transactions() {
  const account = ds.key(['Account', 'a1']);
  await ds.save({ key: account, data: { balance: 100 } });
  const balance = async () => (await ds.get(account))[0].balance;
  await step('read and write in a transaction', async () => {
    const tx = ds.transaction();
    await tx.run();
    const [current] = await tx.get(account);
    tx.save({ key: account, data: { balance: current.balance - 30 } });
    await tx.commit();
    return balance();
  });
  await step('roll back', async () => {
    const tx = ds.transaction();
    await tx.run();
    tx.save({ key: account, data: { balance: 0 } });
    await tx.rollback();
    return balance();
  });
  await step('read only', async () => {
    const tx = ds.transaction({ readOnly: true });
    await tx.run();
    const [current] = await tx.get(account);
    await tx.commit();
    return current.balance;
  });
  await step('a write in a read-only transaction', async () => {
    const tx = ds.transaction({ readOnly: true });
    await tx.run();
    tx.save({ key: account, data: { balance: 5 } });
    await tx.commit();
    return 'ok';
  });
  const bank = ds.key(['Bank', 'b1']);
  await ds.save([1, 2, 3].map((i) => ({ key: ds.key(['Bank', 'b1', 'Acct', i]), data: { n: i } })));
  await step('an ancestor query in a transaction', async () => {
    const tx = ds.transaction();
    await tx.run();
    const [entities] = await tx.runQuery(tx.createQuery('Acct').hasAncestor(bank));
    await tx.rollback();
    return entities.map((e) => e.n);
  });
  await step('a count in a transaction', async () => {
    const tx = ds.transaction();
    await tx.run();
    const [[result]] = await tx.runAggregationQuery(tx.createAggregationQuery(tx.createQuery('Acct').hasAncestor(bank)).count('n'));
    await tx.rollback();
    return result.n;
  });
  await step('a missing entity in a transaction', async () => {
    const tx = ds.transaction();
    await tx.run();
    const [e] = await tx.get(ds.key(['Account', 'nobody']));
    await tx.rollback();
    return entity(e);
  });
  await step('commit twice', async () => {
    const tx = ds.transaction();
    await tx.run();
    tx.save({ key: account, data: { balance: 50 } });
    await tx.commit();
    await tx.commit();
    return 'ok';
  });
  await step('after it, the balance', balance);
  await step('an incomplete key in a transaction', async () => {
    const tx = ds.transaction();
    await tx.run();
    const key = ds.key(['Account']);
    tx.save({ key, data: { balance: 1 } });
    await tx.commit();
    return { complete: key.id !== undefined, read: (await ds.get(key))[0].balance };
  });
}

await { entities, queries, transactions }[section]();
console.log(JSON.stringify(out, null, 1));
process.exit(0);
