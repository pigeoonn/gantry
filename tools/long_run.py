#!/usr/bin/env python3
"""Runs Gantry (empty home, no network, stubbed programs, never a dialog) for a while under light load and reports how
memory, open files and threads change.

    python3 tools/long_run.py target/release/rig [minutes]
"""
import json, os, random, re, shutil, subprocess, sys, tempfile, time
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from fuzz_api import Rig

READ_ONLY = ["/api/state", "/api/stats", "/api/store?per=20", "/api/welcome", "/api/downloads", "/api/conflict_view", "/api/perf", "/api/auto_backups", "/api/health", "/api/disk"]


def sample(pid):
    status = Path(f"/proc/{pid}/status").read_text()
    rss = int(re.search(r"VmRSS:\s+(\d+)", status).group(1)) // 1024
    threads = int(re.search(r"Threads:\s+(\d+)", status).group(1))
    return rss, len(os.listdir(f"/proc/{pid}/fd")), threads


def main():
    binary = str(Path(sys.argv[1]).resolve())
    minutes = float(sys.argv[2]) if len(sys.argv) > 2 else 10
    rig = Rig(binary)
    rig.reseed(force=True)
    end, nxt, rows, n = time.time() + minutes * 60, 0, [], 0
    while time.time() < end:
        for path in random.sample(READ_ONLY, 3):
            try:
                rig.req("GET" if "?" in path or path in ("/api/stats", "/api/downloads", "/api/welcome", "/api/conflict_view", "/api/perf", "/api/auto_backups", "/api/health", "/api/disk") else "POST", path, b"{}")
                n += 1
            except Exception as e:
                print("request failed:", path, e)
        if time.time() >= nxt:
            nxt = time.time() + 30
            rows.append((round((time.time() - (end - minutes * 60))),) + sample(rig.proc.pid))
            print("t=%4ds rss=%4d MB fds=%3d threads=%3d" % rows[-1], flush=True)
        if not rig.alive():
            print("PROBLEM: not answering"); break
        time.sleep(0.5)
    first, last = rows[1] if len(rows) > 1 else rows[0], rows[-1]
    print(f"{n} requests. rss {first[1]} -> {last[1]} MB, fds {first[2]} -> {last[2]}, threads {first[3]} -> {last[3]}")
    bad = rig.panics() or last[1] > first[1] * 1.5 + 30 or last[2] > first[2] + 20 or last[3] > first[3] + 20
    rig.stop()
    shutil.rmtree(rig.home, ignore_errors=True)
    print("FAILED" if bad else "ok: no growth worth worrying about")
    sys.exit(1 if bad else 0)


main()
