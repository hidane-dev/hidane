// How does the official emulator run the Write stream? (#20, #84)
//
// Usage (needs @grpc/grpc-js and @grpc/proto-loader; run outside the repository, see README):
//   node write_stream.mjs 127.0.0.1:8086 <repo>/proto > <repo>/crates/hidane/tests/fixtures/write_stream.json
//
// The scenarios are data: the fixture keeps each scenario's steps next to the official outcome,
// and crates/hidane/tests/write_stream.rs runs the same steps against hidane over gRPC. Each
// scenario uses its own project. Every stream carries `google-cloud-resource-prefix`, as the
// SDKs send it: without it the official emulator fails the stream with UNKNOWN.
//
// Steps:
//   send   {database?, streamId?, token?: "last" | "<literal>", writes?, labels?}
//          database: true = the scenario's database, or a literal name
//          writes: [{update, n?, exists?, reserved?, increment?, serverTime?}]
//   recv   {label}  the next response, error, end of stream, or nothing within 3 s
//   close  {}       half-close the request side
//   lock   {document}  begin a transaction and read the document, holding its lock
//   get    {label, document}  GetDocument after the stream
// Outcomes record responses as {streamId: present, streamToken (text), writeResults:
// [{updateTime: present, transformResults: count}], commitTime: present}, errors as
// {status, message} (project replaced by {project}), and the elapsed time rounded to 0.5 s.
import grpc from '@grpc/grpc-js';
import loader from '@grpc/proto-loader';

const [host, protoDir] = process.argv.slice(2);
const def = loader.loadSync('google/firestore/v1/firestore.proto', {
  includeDirs: [protoDir], keepCase: false, longs: String, enums: String, defaults: false, oneofs: true,
});
const v1 = grpc.loadPackageDefinition(def).google.firestore.v1;
const client = new v1.Firestore(host, grpc.credentials.createInsecure());
const RUN = new Date().toISOString().slice(11, 19).replace(/:/g, '');
const CODES = Object.fromEntries(Object.entries(grpc.status).map(([k, v]) => [v, k]));

const up = (name, n, extra = {}) => ({ update: name, ...(n === undefined ? {} : { n }), ...extra });
const step = (op, fields = {}) => ({ op, ...fields });
const HS = step('send', { database: true });
const scenario = (name, steps) => ({ name, steps });

const SCENARIOS = [
  scenario('handshake, then batches, then an empty request that ends the stream', [
    HS, step('recv', { label: 'handshake' }),
    step('send', { token: 'last', writes: [up('c/a', 1)] }), step('recv', { label: 'one write' }),
    step('send', { token: 'last', writes: [up('c/b', 1), up('c/c', 1)] }), step('recv', { label: 'two writes' }),
    step('send', { token: 'last', writes: [] }), step('recv', { label: 'empty request' }),
    step('recv', { label: 'after the empty request' }),
    step('get', { label: 'c/b was written', document: 'c/b' }),
  ]),
  scenario('pipelined batches are answered in order', [
    HS, step('recv', { label: 'handshake' }),
    step('send', { token: 'last', writes: [up('c/p1', 1)] }),
    step('send', { token: 'last', writes: [up('c/p2', 1)] }),
    step('send', { token: 'last', writes: [up('c/p3', 1)] }),
    step('recv', { label: 'first' }), step('recv', { label: 'second' }), step('recv', { label: 'third' }),
    step('close'), step('recv', { label: 'after half-close' }),
  ]),
  scenario('half-close right after the handshake', [
    HS, step('recv', { label: 'handshake' }), step('close'), step('recv', { label: 'after half-close' }),
  ]),
  scenario('an empty request in the middle ends the stream', [
    HS, step('recv', { label: 'handshake' }),
    step('send', { token: 'last', writes: [] }), step('recv', { label: 'empty request' }),
    step('recv', { label: 'after the empty request' }),
  ]),
  scenario('stream tokens are not checked', [
    HS, step('recv', { label: 'handshake' }),
    step('send', { writes: [up('c/t1', 1)] }), step('recv', { label: 'no token' }),
    step('send', { token: 'bogus', writes: [up('c/t2', 1)] }), step('recv', { label: 'bogus token' }),
    step('close'), step('recv', { label: 'after half-close' }),
  ]),
  scenario('the first request must not carry writes', [
    step('send', { database: true, writes: [up('c/x', 1)] }), step('recv', { label: 'first request' }), step('recv', { label: 'then' }),
  ]),
  scenario('the first request must name the database', [
    step('send', { writes: [] }), step('recv', { label: 'first request' }), step('recv', { label: 'then' }),
  ]),
  scenario('streams cannot be resumed with an ID', [
    step('send', { database: true, streamId: '1', token: '1' }), step('recv', { label: 'first request' }), step('recv', { label: 'then' }),
  ]),
  scenario('streams cannot be resumed with a token', [
    step('send', { database: true, token: '5' }), step('recv', { label: 'first request' }), step('recv', { label: 'then' }),
  ]),
  scenario('later requests may repeat the database', [
    HS, step('recv', { label: 'handshake' }),
    step('send', { database: true, token: 'last', writes: [up('c/d', 1)] }), step('recv', { label: 'write' }),
    step('close'), step('recv', { label: 'after half-close' }),
  ]),
  scenario('later requests must not name another database', [
    HS, step('recv', { label: 'handshake' }),
    step('send', { database: 'projects/other/databases/(default)', token: 'last', writes: [up('c/d', 1)] }),
    step('recv', { label: 'write' }), step('recv', { label: 'then' }),
  ]),
  scenario('a failed precondition ends the stream', [
    HS, step('recv', { label: 'handshake' }),
    step('send', { token: 'last', writes: [up('c/missing', 1, { exists: true })] }),
    step('recv', { label: 'write', compare: 'status' }), step('recv', { label: 'then' }),
  ]),
  scenario('an invalid write ends the stream', [
    HS, step('recv', { label: 'handshake' }),
    step('send', { token: 'last', writes: [up('c/r', 1, { reserved: true })] }),
    step('recv', { label: 'write' }), step('recv', { label: 'then' }),
  ]),
  scenario('transforms report their results', [
    HS, step('recv', { label: 'handshake' }),
    step('send', { token: 'last', writes: [up('c/t', undefined, { increment: 'n', serverTime: 'at' })] }),
    step('recv', { label: 'write' }), step('close'), step('recv', { label: 'after half-close' }),
  ]),
  scenario('labels are accepted', [
    HS, step('recv', { label: 'handshake' }),
    step('send', { token: 'last', writes: [up('c/l', 1)], labels: { a: 'b' } }), step('recv', { label: 'write' }),
    step('close'), step('recv', { label: 'after half-close' }),
  ]),
  scenario('a write waits for transaction locks', [
    step('lock', { document: 'c/lock' }),
    HS, step('recv', { label: 'handshake' }),
    step('send', { token: 'last', writes: [up('c/lock', 1)] }), step('recv', { label: 'write' }),
    step('recv', { label: 'then' }),
  ]),
];

function writeProto(documents, w) {
  const fields = {};
  if (w.n !== undefined) fields.n = { integerValue: String(w.n) };
  if (w.reserved) fields.__x__ = { nullValue: 'NULL_VALUE' };
  const out = { update: { name: `${documents}/${w.update}`, fields } };
  if (w.exists !== undefined) out.currentDocument = { exists: w.exists };
  const transforms = [];
  if (w.increment) transforms.push({ fieldPath: w.increment, increment: { integerValue: '1' } });
  if (w.serverTime) transforms.push({ fieldPath: w.serverTime, setToServerValue: 'REQUEST_TIME' });
  if (transforms.length) out.updateTransforms = transforms;
  return out;
}

async function run(sc, index) {
  const project = `ws-${RUN}-${index}`;
  const database = `projects/${project}/databases/(default)`;
  const documents = `${database}/documents`;
  const md = new grpc.Metadata();
  md.set('google-cloud-resource-prefix', database);
  const call = client.write(md);
  const queue = [];
  const waiters = [];
  const push = (e) => { const w = waiters.shift(); if (w) w(e); else queue.push(e); };
  call.on('data', (r) => push({ data: r }));
  call.on('error', (e) => push({ error: e }));
  call.on('end', () => push({ end: true }));
  const next = () => (queue.length ? Promise.resolve(queue.shift())
    : new Promise((res) => { waiters.push(res); setTimeout(() => res({ timeout: true }), 3000); }));
  let last;
  const steps = [];
  for (const st of sc.steps) {
    const out = { ...st };
    if (st.op === 'send') {
      const req = {};
      if (st.database === true) req.database = database;
      else if (st.database) req.database = st.database;
      if (st.streamId) req.streamId = st.streamId;
      if (st.token === 'last') req.streamToken = last;
      else if (st.token) req.streamToken = Buffer.from(st.token);
      if (st.writes) req.writes = st.writes.map((w) => writeProto(documents, w));
      if (st.labels) req.labels = st.labels;
      call.write(req);
    } else if (st.op === 'recv') {
      const start = Date.now();
      const e = await next();
      const elapsed = Math.round((Date.now() - start) / 500) / 2;
      if (e.data) {
        const r = e.data;
        if (r.streamToken) last = r.streamToken;
        out.outcome = { response: {
          streamId: !!r.streamId,
          streamToken: r.streamToken ? r.streamToken.toString() : null,
          writeResults: (r.writeResults || []).map((w) => ({ updateTime: !!w.updateTime, transformResults: (w.transformResults || []).length })),
          commitTime: !!r.commitTime,
        }, elapsed };
      } else if (e.error) {
        out.outcome = { error: { status: CODES[e.error.code], message: (e.error.details || '').replaceAll(project, '{project}') }, elapsed };
      } else if (e.end) {
        out.outcome = { end: true, elapsed };
      } else {
        out.outcome = { timeout: true };
      }
    } else if (st.op === 'close') {
      call.end();
    } else if (st.op === 'lock') {
      const tx = await new Promise((res, rej) => client.beginTransaction({ database }, md, (e, r) => (e ? rej(e) : res(r.transaction))));
      await new Promise((res) => {
        const c = client.batchGetDocuments({ database, documents: [`${documents}/${st.document}`], transaction: tx }, md);
        c.on('data', () => {}); c.on('end', res); c.on('error', res);
      });
    } else if (st.op === 'get') {
      out.outcome = await new Promise((res) => client.getDocument({ name: `${documents}/${st.document}` }, md, (e, d) => res(e
        ? { error: { status: CODES[e.code], message: e.details.replaceAll(project, '{project}') } }
        : { document: { n: d.fields?.n ? Number(d.fields.n.integerValue) : null } })));
    }
    steps.push(out);
  }
  call.cancel();
  return { name: sc.name, steps };
}

const scenarios = [];
for (const [i, sc] of SCENARIOS.entries()) {
  process.stderr.write(`${sc.name}\n`);
  scenarios.push(await run(sc, i));
}
console.log(JSON.stringify({
  oracle: 'cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)',
  generated_by: 'tools/oracle/write_stream.mjs',
  scenarios,
}, null, 1));
client.close();
process.exit(0);
