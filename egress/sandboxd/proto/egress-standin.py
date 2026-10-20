#!/usr/bin/env python3
"""Stand-in for the egress role during the prototype (the real one is chat/src/egress): an HTTP forward proxy on
:3128 (CONNECT and absolute-URI requests; the sbx token in Proxy-Authorization is accepted, not checked), plus a
heartbeat server on 127.0.0.1:9000 reached THROUGH the proxy, which logs every gap in a client's heartbeats as seen
from outside the sandbox (the downtime a peer of the sandbox observes). Usage: egress-standin.py LOGFILE"""
import asyncio, sys, time

LOG = open(sys.argv[1] if len(sys.argv) > 1 else "/var/log/sbx-heartbeat.log", "a", buffering=1)

async def pipe(r, w):
    try:
        while (b := await r.read(65536)):
            w.write(b); await w.drain()
    except Exception:
        pass
    finally:
        w.close()

async def proxy(r, w):
    try:
        head = await r.readuntil(b"\r\n\r\n")
    except Exception:
        w.close(); return
    line = head.split(b"\r\n", 1)[0].decode()
    method, target, _ = line.split(" ", 2)
    if method == "CONNECT":
        host, port = target.rsplit(":", 1)
        try:
            ur, uw = await asyncio.open_connection(host, int(port))
        except Exception as e:
            w.write(f"HTTP/1.1 502 Bad Gateway\r\n\r\n{e}\r\n".encode()); w.close(); return
        w.write(b"HTTP/1.1 200 Connection established\r\n\r\n"); await w.drain()
    else:
        from urllib.parse import urlsplit
        u = urlsplit(target)
        ur, uw = await asyncio.open_connection(u.hostname, u.port or 80)
        path = (u.path or "/") + ("?" + u.query if u.query else "")
        rest = b"\r\n".join(l for l in head.split(b"\r\n")[1:] if not l.lower().startswith(b"proxy-"))
        uw.write(f"{method} {path} HTTP/1.1\r\n".encode() + rest)
    await asyncio.gather(pipe(r, uw), pipe(ur, w))

async def heartbeat(r, w):
    """The client sends a line every 20 ms; each line is echoed. Logs the largest gap per connection, live."""
    peer = w.get_extra_info("peername"); last = time.monotonic(); n = 0; worst = 0.0
    LOG.write(f"{time.time():.3f} open {peer}\n")
    try:
        while (line := await r.readline()):
            now = time.monotonic(); gap = (now - last) * 1000; last = now; n += 1
            if gap > 100:
                LOG.write(f"{time.time():.3f} gap {gap:.0f} ms after line {n} ({line.strip().decode()})\n")
            worst = max(worst, gap)
            w.write(line); await w.drain()
    except Exception as e:
        LOG.write(f"{time.time():.3f} error {e!r}\n")
    LOG.write(f"{time.time():.3f} close {peer} lines={n} worst_gap_ms={worst:.0f}\n")

async def main():
    await asyncio.start_server(proxy, "0.0.0.0", 3128)
    await asyncio.start_server(heartbeat, "127.0.0.1", 9000)
    await asyncio.Event().wait()

asyncio.run(main())
