#!/usr/bin/env python3
"""Runs inside the sandbox during the migration test (started in the background through the agent's /exec):
  mem   SIZE_MIB RATE_MIB_S  fill a SIZE buffer, then keep dirtying RATE MiB/s of it (random 4 KiB pages);
                             every second appends "time pages_written checksum" to /work/mem.log
  heart HOST PORT            through the HTTP proxy in $HTTPS_PROXY, CONNECT to HOST:PORT and send a numbered line
                             every 20 ms, reading each echo back; logs to /work/heart.log
"""
import os, random, socket, sys, time, base64
from urllib.parse import urlsplit

def mem(size_mib, rate_mib):
    size = size_mib << 20; buf = bytearray(size); page = 4096; pages = size // page
    for i in range(0, size, page):
        buf[i] = 1
    log = open("/work/mem.log", "a", buffering=1); written = 0; t_next = time.time() + 1
    per_tick = max(1, (rate_mib << 20) // page // 100)   # 100 ticks per second
    while True:
        for _ in range(per_tick):
            p = random.randrange(pages) * page
            buf[p:p + 8] = written.to_bytes(8, "little"); written += 1
        time.sleep(0.01)
        if time.time() >= t_next:
            t_next += 1; log.write(f"{time.time():.3f} {written} {buf[0]}\n")

def heart(host, port):
    u = urlsplit(os.environ["HTTPS_PROXY"])
    s = socket.create_connection((u.hostname, u.port or 3128))
    auth = base64.b64encode(f"{u.username}:{u.password}".encode()).decode()
    s.sendall(f"CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\nProxy-Authorization: Basic {auth}\r\n\r\n".encode())
    resp = b""
    while b"\r\n\r\n" not in resp:
        resp += s.recv(1)
    f = s.makefile("rb"); log = open("/work/heart.log", "a", buffering=1); n = 0
    log.write(f"{time.time():.3f} connected {resp.split(b'\r\n')[0].decode()}\n")
    while True:
        n += 1; t = time.time(); s.sendall(f"{n} {t:.3f}\n".encode())
        echo = f.readline()
        if not echo:
            log.write(f"{time.time():.3f} closed after {n}\n"); return
        rtt = (time.time() - t) * 1000
        if rtt > 100:
            log.write(f"{time.time():.3f} slow echo {n} rtt={rtt:.0f}ms\n")
        if n % 500 == 0:
            log.write(f"{time.time():.3f} alive {n}\n")
        time.sleep(0.02)

if __name__ == "__main__":
    if sys.argv[1] == "mem":
        mem(int(sys.argv[2]), int(sys.argv[3]))
    else:
        heart(sys.argv[2], int(sys.argv[3]))
