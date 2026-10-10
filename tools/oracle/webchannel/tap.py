"""A TCP proxy that records every read in both directions with a timestamp, as JSON lines.

Usage:
    python3 -I tools/oracle/webchannel/tap.py 8095 127.0.0.1:8086 capture.jsonl

Put it between a browser and an emulator to see the WebChannel wire as sent, chunk boundaries
included (docs/webchannel.md was written from such captures). Captures stay out of the
repository: they hold session IDs and local times.
"""
import asyncio, json, sys, time, itertools
LISTEN, TARGET, LOG = int(sys.argv[1]), sys.argv[2], sys.argv[3]
host, port = TARGET.split(":")
ids = itertools.count(1)
start = time.monotonic()
out = open(LOG, "a", buffering=1)

async def pump(conn, direction, reader, writer):
    try:
        while data := await reader.read(65536):
            out.write(json.dumps({"t": round(time.monotonic() - start, 4), "conn": conn, "dir": direction, "data": data.decode("latin-1")}) + "\n")
            writer.write(data)
            await writer.drain()
    except (ConnectionError, asyncio.CancelledError):
        pass
    finally:
        out.write(json.dumps({"t": round(time.monotonic() - start, 4), "conn": conn, "dir": direction, "eof": True}) + "\n")
        try:
            writer.close()
        except Exception:
            pass

async def handle(cr, cw):
    conn = next(ids)
    sr, sw = await asyncio.open_connection(host, int(port))
    await asyncio.gather(pump(conn, ">", cr, sw), pump(conn, "<", sr, cw))

async def main():
    server = await asyncio.start_server(handle, "127.0.0.1", LISTEN)
    async with server:
        await server.serve_forever()
asyncio.run(main())
