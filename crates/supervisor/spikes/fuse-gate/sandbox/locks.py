#!/usr/bin/env python3
"""flock and fcntl (POSIX) locks between two processes on a file, as on a local file. usage: locks.py PATH"""
import fcntl, json, os, sys, time

def probe(path):
    res = {}
    fd = os.open(path, os.O_RDWR | os.O_CREAT, 0o600)
    for kind in ("flock", "fcntl"):
        r, w = os.pipe()
        r2, w2 = os.pipe()
        pid = os.fork()
        if pid == 0:
            cfd = os.open(path, os.O_RDWR)
            if kind == "flock":
                fcntl.flock(cfd, fcntl.LOCK_EX)
            else:
                fcntl.lockf(cfd, fcntl.LOCK_EX, 10, 0)
            os.write(w, b"1")
            os.read(r2, 1)
            os._exit(0)
        os.read(r, 1)
        try:
            if kind == "flock":
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            else:
                fcntl.lockf(fd, fcntl.LOCK_EX | fcntl.LOCK_NB, 10, 0)
            contended = "acquired (NOT exclusive)"
        except OSError as e:
            contended = f"refused errno {e.errno}"
        if kind == "fcntl":
            # F_GETLK through struct flock: who holds it
            import struct
            fl = struct.pack("hhqqi", fcntl.F_WRLCK, 0, 0, 10, 0)
            got = fcntl.fcntl(fd, fcntl.F_GETLK, fl)
            l_type, _, _, _, l_pid = struct.unpack("hhqqi", got)
            contended += f"; F_GETLK type={l_type} pid={l_pid} child={pid}"
        os.write(w2, b"1")
        os.waitpid(pid, 0)
        t = time.time()
        if kind == "flock":
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            fcntl.flock(fd, fcntl.LOCK_UN)
        else:
            fcntl.lockf(fd, fcntl.LOCK_EX | fcntl.LOCK_NB, 10, 0)
            fcntl.lockf(fd, fcntl.LOCK_UN, 10, 0)
        res[kind] = {"while_child_holds": contended, "after_child_exit": "acquired"}
    os.close(fd)
    return res

print(json.dumps({"path": sys.argv[1], "result": probe(sys.argv[1])}))
