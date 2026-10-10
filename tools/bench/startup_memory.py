"""Time until hidane's port accepts, and its resident memory idle and after 1,000 documents.

Usage (after `cargo build --release`):
    python3 -I tools/bench/startup_memory.py target/release/hidane > results/startup-memory-hidane.txt

Starts the binary 10 times on free ports and measures the time until a TCP connect succeeds,
then keeps the last process, reads its RSS after 1 s idle, writes 1,000 documents shaped like the
official baseline (`users/{i}` = `{mykey, myid}`, 500 per commit, over REST) and reads it again.
The official emulator's numbers for the same measurements are in
`results/official-v1.22.0-baseline.md`.
"""

import http.client
import json
import platform
import socket
import statistics
import subprocess
import sys
import time

BIN = sys.argv[1]
DB = "projects/demo-hidane/databases/(default)"


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def rss_mib(pid):
    out = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True)
    return int(out.stdout.strip()) / 1024


times = []
for i in range(10):
    port = free_port()
    start = time.perf_counter()
    process = subprocess.Popen([BIN, "--host", "127.0.0.1", "--port", str(port)],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    while True:
        try:
            socket.create_connection(("127.0.0.1", port), timeout=0.05).close()
            break
        except OSError:
            time.sleep(0.001)
    times.append((time.perf_counter() - start) * 1000)
    if i < 9:
        process.terminate()
        process.wait()

time.sleep(1)
idle = rss_mib(process.pid)
for batch in range(2):
    writes = [{"update": {"name": f"{DB}/documents/users/{n}",
                          "fields": {"mykey": {"stringValue": f"key{n}"}, "myid": {"integerValue": str(n)}}}}
              for n in range(batch * 500, batch * 500 + 500)]
    c = http.client.HTTPConnection("127.0.0.1", port)
    c.request("POST", f"/v1/{DB}/documents:commit", body=json.dumps({"writes": writes}),
              headers={"Content-Type": "application/json"})
    response = c.getresponse()
    response.read()
    assert response.status == 200
time.sleep(1)
after = rss_mib(process.pid)
process.terminate()
process.wait()

print(f"# tools/bench/startup_memory.py, release build, {platform.system()} {platform.machine()}")
print(f"time_to_accept_ms_median,{statistics.median(times):.1f}")
print(f"time_to_accept_ms_runs,{' '.join(f'{t:.1f}' for t in times)}")
print(f"rss_idle_mib,{idle:.1f}")
print(f"rss_after_1000_documents_mib,{after:.1f}")
