#!/usr/bin/env python3
"""Throws malformed requests at every endpoint of a running Gantry and checks that it stays alive.

    python3 tools/fuzz_api.py target/release/rig [rounds]

A fresh empty HOME is used, no display is reachable and the programs Gantry may start (browser, xdg-open, notify-send,
steam, file dialogs...) are replaced by stubs that only write down that they were called, so nothing happens on the
machine it runs on. Fails (exit 1) when the process dies, prints a panic, stops answering, answers slowly, or a stub
was called. Endpoints are read from `src/main.rs`.
"""
import http.client
import json
import os
import random
import re
import shutil
import signal
import struct
import zipfile
import io
import subprocess
import sys
import tempfile
import urllib.parse
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
STUBS = ["xdg-open", "brave", "brave-browser", "google-chrome", "chromium", "chromium-browser", "microsoft-edge", "notify-send",
         "steam", "kdialog", "zenity", "gamemoderun", "mangohud", "xdg-mime", "update-desktop-database", "wine", "protontricks"]
SITES = tuple(f"xdg-open https://{h}/" for h in ("www.nexusmods.com", "thunderstore.io", "steamcommunity.com", "github.com"))
SKIP = {"/api/subscribe", "/api/quit", "/api/quit_now"}  # a long poll that is meant to wait, and the ones that end the program


def endpoints():
    """Every endpoint, except the ones that open a file dialog (they reach the real desktop through D-Bus however the
    environment is cleaned, and a test must never put windows on someone's screen) and the ones in SKIP."""
    src = (ROOT / "src" / "main.rs").read_text().split("#[cfg(test)]")[0]
    arm, found, dialogs = None, set(), set()
    for line in src.splitlines():
        m = re.match(r'^\s*"(/api/[a-z0-9_]+)"\s*(?:\||=>)', line)
        if m:
            arm = m.group(1)
            found.add(arm)
        if arm and "rfd::" in line:
            dialogs.add(arm)
    assert dialogs, "no dialog endpoints found: the scan is broken, refusing to run"
    return sorted(found - dialogs - SKIP)


def bodies(rng, ids=(), appids=()):
    big = "A" * 200_000
    deep = "[" * 3000 + "]" * 3000
    yield b"{}"
    yield b"[]"
    yield b"null"
    yield b'"text"'
    yield b"12345"
    yield b""
    yield b"{"
    yield b"\xff\xfe\x00garbage"
    yield deep.encode()
    yield json.dumps({"id": big, "name": big, "path": big, "key": big, "url": big}).encode()
    yield json.dumps({"id": 1, "index": -1, "n": 1e30, "delta": 2 ** 70, "enabled": "yes", "on": [], "appid": None}).encode()
    yield json.dumps({"path": "../../../../etc/passwd", "name": "../../x", "dir": "/", "id": "../..", "file": "\\\\server\\share"}).encode()
    yield json.dumps({"id": "a\u0000b", "name": "\n\r\t", "url": "javascript:alert(1)", "key": "‮\u0000"}).encode()
    yield json.dumps({"ids": [1, "a", None, {}], "paths": "x", "mods": 5, "list": {"a": 1}}).encode()
    for sid in list(ids)[:4]:
        yield json.dumps({"id": sid}).encode()
        yield json.dumps({"id": sid, "enabled": False, "delta": rng.choice([-99, 99, 0]), "note": "n" * 300, "group": "g" * 300, "path": "Data/shared.esp"}).encode()
        yield json.dumps({"ids": [sid, "nope"], "winner": sid, "over": "", "dir": sid, "name": sid, "rule": "auto"}).encode()
    for a in list(appids)[:2]:
        yield json.dumps({"appid": a, "index": 0, "name": "x", "tab": "mods"}).encode()
    for _ in range(4):
        yield json.dumps({rng.choice(["id", "name", "path", "index", "value", "what", "key", "tab", "theme", "lang"]): rng.choice([0, -1, 3.5, "x" * rng.randint(0, 5000), True, None, [1, 2], {"a": 1}])}).encode()


def queries(rng):
    yield ""
    yield "?q=" + "A" * 50_000
    yield "?id=../../etc/passwd&q=%00&appid=%ff&n=-1&off=99999999999999999999"
    yield "?source=nexus&id=" + "9" * 400
    yield "?lang=../../x&tz=NaN&per=-5&all=1&sort=%27%22%3C"
    yield "?" + "&".join(f"k{i}=v{i}" for i in range(300))


def zip_bytes(entries):
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
        for item in entries:
            if isinstance(item, tuple):
                name, data = item
                info = zipfile.ZipInfo(name)
                info.external_attr = 0o100644 << 16
                z.writestr(info, data)
    return buf.getvalue()


def symlink_zip():
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as z:
        info = zipfile.ZipInfo("Data/link.esp")
        info.external_attr = 0o120777 << 16
        z.writestr(info, "/etc/passwd")
        z.writestr("Data/ok.esp", "ok")
    return buf.getvalue()


def mods():
    """Archives that are fine and archives that are not."""
    return {
        "Good Pack-1-1-0-1700000000.zip": zip_bytes([("Data/new.esp", "NEW"), ("helper.exe", "MZ"), ("readme.txt", "read me"), ("Data/shared.esp", "A")]),
        "Other Pack-2-1-0-1700000000.zip": zip_bytes([("Data/shared.esp", "B"), ("Data/other.esp", "O")]),
        "Slip.zip": zip_bytes([("../../evil.txt", "x"), ("/abs/evil.txt", "x"), ("a/../../b.txt", "x"), ("C:\\win\\evil.txt", "x"), ("Data/fine.esp", "x")]),
        "Many.zip": zip_bytes([(f"Data/f{i}.esp", "x") for i in range(1500)]),
        "Link.zip": symlink_zip(),
        "Fomod.zip": zip_bytes([("fomod/ModuleConfig.xml", "<config><oops"), ("fomod/info.xml", "<fomod><Name>x</Name></fomod>"), ("Data/a.esp", "x")]),
        "Bomb.zip": zip_bytes([("Data/zeros.bin", b"\0" * 60_000_000)]),
        "Truncated.zip": zip_bytes([("Data/a.esp", "x" * 5000)])[:-40],
        "Empty.zip": zip_bytes([]),
        "Weird name \u202e\u0000.zip": zip_bytes([("Data/w.esp", "w")]),
    }


class Rig:
    def __init__(self, binary):
        self.home = Path(tempfile.mkdtemp(prefix="gantry-fuzz-"))
        self.bin = self.home / "stubs"
        self.bin.mkdir()
        self.calls = self.home / "calls.log"
        for name in STUBS:
            (self.bin / name).write_text(f'#!/bin/sh\necho "{name} $@" >> "{self.calls}"\nexit 0\n')
            (self.bin / name).chmod(0o755)
        game = self.home / "games" / "common" / "Fuzz Game"
        (game / "Data").mkdir(parents=True)
        (game / "Data" / "base.esp").write_text("ORIGINAL")
        (game / "Data" / "shared.esp").write_text("VANILLA")
        (game / "fuzz.exe").write_bytes(b"MZ")
        cfg = self.home / ".config" / "protonrig"
        cfg.mkdir(parents=True)
        (cfg / "custom_games.json").write_text(json.dumps([{"appid": "custom_fuzz", "name": "Fuzz Game", "game_dir": str(game), "exe_path": str(game / "fuzz.exe"), "engine": "Standard / Non-UE", "is_custom": True}]))
        self.game = game
        self.port = random.randint(19900, 19990)
        env = {k: v for k, v in os.environ.items() if k not in ("DISPLAY", "WAYLAND_DISPLAY", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS")}
        # every network call goes to a closed port: nothing reaches a real server
        env.update(ALL_PROXY="http://127.0.0.1:9", HTTP_PROXY="http://127.0.0.1:9", HTTPS_PROXY="http://127.0.0.1:9", NO_PROXY="127.0.0.1", HOME=str(self.home), PATH=f"{self.bin}:{os.environ.get('PATH', '')}", RIG_PORT=str(self.port), RIG_NO_WINDOW="1",
                   XDG_CONFIG_HOME=str(self.home / ".config"), XDG_DATA_HOME=str(self.home / ".local/share"), XDG_CACHE_HOME=str(self.home / ".cache"))
        self.err = open(self.home / "stderr.log", "w")
        self.proc = subprocess.Popen([binary], env=env, stdout=subprocess.DEVNULL, stderr=self.err)
        self.token = ""
        for _ in range(100):
            try:
                page = self.req("GET", "/", token=False)[1].decode("utf8", "replace")
                m = re.search(r'RIG_TOKEN\s*=\s*"([^"]+)"', page)
                if m:
                    self.token = m.group(1)
                    break
            except Exception:
                time.sleep(0.2)
        if not self.token:
            raise SystemExit("Gantry did not start")

    def req(self, method, path, body=b"", token=True, timeout=15, headers=None):
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=timeout)
        h = {"Content-Type": "application/json"}
        if token:
            h["X-Rig-Token"] = self.token
        h.update(headers or {})
        c.request(method, path, body=body if method != "GET" else None, headers=h)
        r = c.getresponse()
        data = r.read()
        c.close()
        return r.status, data

    def state(self):
        return json.loads(self.req("POST", "/api/state", b"{}")[1])

    def reseed(self, force=False):
        """Make sure the library has mods to work on; returns the ids of the mods and games."""
        st = self.state()
        have = [m["id"] for m in (st.get("library") or {}).get("mods", [])]
        if force or len(have) < 3:
            idx = next((i for i, g in enumerate(st["games"]) if g["appid"] == "custom_fuzz"), 0)
            self.req("POST", "/api/select", json.dumps({"index": idx}).encode())
            for name, data in mods().items():
                try:
                    self.req("POST", "/api/drop_upload?name=" + urllib.parse.quote(name), data, timeout=60)
                except Exception:
                    pass
            time.sleep(1.5)
            st = self.state()
            have = [m["id"] for m in (st.get("library") or {}).get("mods", [])]
        return have, [g["appid"] for g in st["games"]]

    def alive(self):
        if self.proc.poll() is not None:
            return False
        try:
            return self.req("POST", "/api/state", b"{}", timeout=10)[0] == 200
        except Exception:
            return False

    def stop(self):
        try:
            self.proc.send_signal(signal.SIGTERM)
            self.proc.wait(timeout=5)
        except Exception:
            self.proc.kill()
        self.err.close()

    def panics(self):
        return [l for l in (self.home / "stderr.log").read_text(errors="replace").splitlines() if "panicked" in l]

    def stub_calls(self):
        return self.calls.read_text().splitlines() if self.calls.exists() else []


def main():
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    binary = str(Path(sys.argv[1]).resolve())
    rounds = int(sys.argv[2]) if len(sys.argv) > 2 else 1
    rng = random.Random(1234)
    rig = Rig(binary)
    problems, slow, sent = [], [], 0
    eps = endpoints()
    print(f"{len(eps)} endpoints, {rounds} round(s)")
    try:
        # the doors: no token, a wrong token, a foreign host and origin
        for path in eps[:20]:
            for kw in ({"token": False}, {"headers": {"X-Rig-Token": "wrong"}}, {"headers": {"Host": "evil.example:80"}}, {"headers": {"Origin": "https://evil.example"}}):
                for method in ("GET", "POST"):
                    code, _ = rig.req(method, path, b"{}", **kw)
                    if code == 200:
                        problems.append(f"{method} {path} answered 200 with {list(kw)}")
        ids, appids = rig.reseed(force=True)
        print(f"seeded: {len(ids)} mods, {len(appids)} game(s)")
        last = {"/api/lib_remove", "/api/lib_remove_many", "/api/delete_mod", "/api/wipe_mods", "/api/purge_all", "/api/remove_game", "/api/restore_game", "/api/lib_import", "/api/backup_restore", "/api/auto_backup_restore", "/api/lib_purge"}
        eps = [e for e in eps if e not in last] + [e for e in eps if e in last]
        for rnd in range(rounds):
            for path in eps:
                ids, appids = rig.reseed()
                for method in ("GET", "POST"):
                    variants = [(path + q, b"") for q in queries(rng)] if method == "GET" else [(path, b) for b in bodies(rng, ids, appids)]
                    for url, body in variants:
                        t0 = time.time()
                        try:
                            code, _ = rig.req(method, url, body)
                        except Exception as e:
                            if not rig.alive():
                                problems.append(f"the process died or stopped answering after {method} {path} ({type(e).__name__}) body={body[:60]!r}")
                                raise SystemExit(report(rig, problems, slow, sent))
                            problems.append(f"{method} {path}: {type(e).__name__}: {e}")
                            continue
                        sent += 1
                        took = time.time() - t0
                        if took > 3:
                            slow.append(f"{method} {path} took {took:.1f}s")
                        if code >= 500:
                            problems.append(f"{method} {path} answered {code} for {body[:60]!r}")
                if not rig.alive():
                    problems.append(f"the process died or stopped answering after {path}")
                    raise SystemExit(report(rig, problems, slow, sent))
                time.sleep(0.05)
        time.sleep(1.5)  # background work started by the requests
        if not rig.alive():
            problems.append("the process was not healthy at the end")
    finally:
        code = report(rig, problems, slow, sent)
        rig.stop()
        shutil.rmtree(rig.home, ignore_errors=True)
    sys.exit(code)


def escapes(rig):
    """Files that a hostile archive or request got outside the places Gantry may write, and links that leave them."""
    out = []
    if Path("/abs/evil.txt").exists():
        out.append("/abs/evil.txt was written")
    for base, dirs, files in os.walk(rig.home):
        inside_library = ".gantry" in Path(base).parts
        for name in files + dirs:
            path = Path(base) / name
            if name in ("evil.txt", "b.txt") and not inside_library:
                out.append(f"{path} was written outside a library")
            if path.is_symlink():
                target = os.path.realpath(path)
                if not target.startswith(str(rig.home)):
                    out.append(f"{path} is a link that leaves the sandbox: {target}")
    # the game folder may only hold what the deployed mods put there (hard links, never links to other places)
    return out


def report(rig, problems, slow, sent):
    panics = rig.panics()
    # play, winecfg and nxm_register call these on purpose (they are stubs here), opening a mod library folder or the game's own pages is what buttons are for
    calls = [c for c in rig.stub_calls() if not c.startswith(("xdg-mime ", "update-desktop-database ", "wine ", "gamemoderun ", "notify-send ", "mangohud ")) and not c.startswith(f"xdg-open {rig.home}/") and not c.startswith(SITES)]
    escaped = escapes(rig)
    print(f"{sent} requests sent")
    for p in panics[:20]:
        print("PANIC:", p)
    for c in sorted(set(calls))[:20]:
        print(f"A program was started ({calls.count(c)}x):", c)
    for s in sorted(set(slow))[:20]:
        print("SLOW:", s)
    for p in sorted(set(problems))[:40]:
        print("PROBLEM:", p)
    for e in escaped[:20]:
        print("ESCAPED:", e)
    bad = bool(panics or calls or problems or escaped)
    print("FAILED" if bad else "ok: nothing died, nothing started, nothing answered 5xx")
    return 1 if bad else 0


if __name__ == "__main__":
    main()
