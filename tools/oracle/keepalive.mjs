// Does the emulator close a connection whose client pings it often? Mobile SDKs keep their
// streams alive with HTTP/2 pings; the iOS SDK reported a GOAWAY after about 90 s against the
// official emulator (firebase-tools #11238). This client pings an idle connection every 10 s
// for 70 s and prints every connectivity change and GOAWAY that grpc-js traces.
//
// Usage (Node.js with `npm i @grpc/grpc-js`, outside the repository):
//   GRPC_TRACE=keepalive,transport GRPC_VERBOSITY=DEBUG node keepalive.mjs 127.0.0.1:8080 \
//     2>&1 | grep -E 'state |GOAWAY|done'
import grpc from '@grpc/grpc-js';

const target = process.argv[2];
const seconds = Number(process.argv[3] ?? 70);
const channel = new grpc.Channel(target, grpc.credentials.createInsecure(), {
  'grpc.keepalive_time_ms': 10000,
  'grpc.keepalive_timeout_ms': 5000,
  'grpc.keepalive_permit_without_calls': 1,
});
const start = Date.now();
const log = (s) => console.log(`${((Date.now() - start) / 1000).toFixed(1)}s ${s}`);
let state = channel.getConnectivityState(true);
let closed = false;
const watch = () => channel.watchConnectivityState(state, Date.now() + seconds * 1000 + 5000, (err) => {
  if (err || closed) return;
  state = channel.getConnectivityState(false);
  log(`state ${grpc.connectivityState[state]}`);
  if (state !== grpc.connectivityState.SHUTDOWN) watch();
});
watch();
setTimeout(() => { closed = true; log('done'); channel.close(); process.exit(0); }, seconds * 1000);
