"""Exercise the installed Mesh CLI against an explicitly selected Git remote.

Creates two isolated clients and synthetic files in a unique remote subfolder.
Does not modify the user's wiki, machine connectivity, or global Git settings.
Local evidence is retained for investigation. No AI service is configured.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import time
from datetime import datetime, timezone
import uuid


class LiveTest:
    def __init__(self, args):
        self.exe = args.exe.resolve()
        self.build = subprocess.check_output([str(self.exe), "--version"], text=True).strip()
        self.binary_hash = hashlib.sha256(self.exe.read_bytes()).hexdigest()
        self.report = args.report.resolve()
        if args.resume:
            self.root = args.resume.resolve()
            context = json.loads((self.root / "context.json").read_text())
            assert context["remote"] == args.remote
            self.run_id = context["run_id"]
            self.results = context.get("results", [])
        else:
            self.root = Path(tempfile.mkdtemp(prefix="dispatch-mesh-live-"))
            self.run_id = "run-" + datetime.now(timezone.utc).strftime("%Y%m%d") + "-" + uuid.uuid4().hex[:8]
            self.results = []
        self.remote = args.remote
        self.prefix = "mesh-tests/" + self.run_id
        self.clients = {name: self.root / name for name in ("alice", "bob")}
        self.processes = {}
        self.save_context()
        print("Evidence directory:", self.root, flush=True)
        print("Remote test prefix:", self.prefix, flush=True)
        for name, folder in self.clients.items():
            if not (folder / ".mesh/config.json").exists():
                self.mesh(name, "init", "--name", "Mesh Test " + name.title(), "--email", name + "@mesh-test.invalid", "--remote", self.remote)

    def save_context(self):
        (self.root / "context.json").write_text(json.dumps({"remote": self.remote, "run_id": self.run_id, "results": self.results}, indent=2), encoding="utf-8")

    def mesh(self, name, *args):
        for _ in range(60):
            result = subprocess.run([str(self.exe), "--workspace", str(self.clients[name]), *args], capture_output=True, text=True, timeout=100)
            if result.returncode == 0:
                return result.stdout
            if "Another Mesh operation" not in result.stderr:
                raise AssertionError(f"{name} {' '.join(args)}: {result.stderr.strip()}")
            time.sleep(.2)
        raise AssertionError(name + " operation lock remained busy")

    def git(self, name, *args):
        result = subprocess.run(["git", "--git-dir", str(self.clients[name] / ".mesh/objects.git"), *args], capture_output=True, timeout=40)
        if result.returncode:
            raise AssertionError(f"Git {args[0]} failed ({result.returncode})")
        return result.stdout

    def state(self, name):
        with sqlite3.connect(self.clients[name] / ".mesh/state.sqlite", timeout=2) as db:
            row = json.loads(db.execute("select value from state where id=1").fetchone()[0])
        return row.get("state", row)

    def captured(self, name, relative, expected):
        with sqlite3.connect(self.clients[name] / ".mesh/state.sqlite", timeout=2) as db:
            row = json.loads(db.execute("select value from state where id=1").fetchone()[0])
            state = row.get("state", row)
            value = state["observed"].get(relative)
            if value is None:
                return expected is None
            if isinstance(value, dict):
                data = db.execute("select content from blobs where id=?", [value["blob"]]).fetchone()[0]
            else:
                data = bytes(value)
        return data == expected

    def start(self, name):
        self.stop(name)
        log_path = self.root / (name + ".log")
        offset = log_path.stat().st_size if log_path.exists() else 0
        log = open(log_path, "ab", buffering=0)
        flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
        self.processes[name] = subprocess.Popen([str(self.exe), "--workspace", str(self.clients[name]), "run"], stdout=log, stderr=log, creationflags=flags)
        log.close()
        started = time.monotonic()
        while True:
            if self.processes[name].poll() is not None:
                raise AssertionError(name + " runner exited during startup")
            if b"Mesh is running for" in log_path.read_bytes()[offset:]:
                break
            assert time.monotonic() - started < 15, name + " runner did not start"
            time.sleep(.1)

    def stop(self, name):
        process = self.processes.pop(name, None)
        if process is None or process.poll() is not None:
            return
        if os.name == "nt":
            subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"], capture_output=True, timeout=15)
        else:
            process.terminate()
        process.wait(timeout=15)

    def wait(self, label, predicate, timeout=100):
        start = time.monotonic()
        heartbeat = start
        while True:
            for name, process in self.processes.items():
                assert process.poll() is None, name + " runner exited unexpectedly; inspect its retained log"
            if predicate():
                return round(time.monotonic() - start, 2)
            if time.monotonic() - start > timeout:
                details = {name: {"attention": self.state(name).get("attention"), "unresolved": len(self.conflicts(name))} for name in self.clients}
                raise AssertionError(f"Timed out: {label}; {details}")
            if time.monotonic() - heartbeat > 30:
                print(f"  Waiting for {label}: {round(time.monotonic()-start)}s", flush=True)
                heartbeat = time.monotonic()
            time.sleep(.3)

    def path(self, leaf):
        return self.prefix + "/" + leaf

    def write(self, name, relative, data):
        path = self.clients[name] / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)

    def received(self, name, relative, data):
        path = self.clients[name] / relative
        return path.is_file() and path.read_bytes() == data

    def active(self, name):
        config = json.loads((self.clients[name] / ".mesh/config.json").read_text())
        return next((s for s in self.state(name)["manifest"]["sessions"].values() if s["device"] == config["device"] and not s["closed"] and not s["published"] and not s.get("discarded")), None)

    def conflicts(self, name):
        state = self.state(name)
        return [c for key, c in state["conflicts"].items() if key not in state["manifest"]["resolutions"]]

    def finish(self, name):
        for _ in range(3):
            result = json.loads(self.mesh(name, "session", "finish"))
            if self.active(name) is None:
                return result["session_id"]
        raise AssertionError("Could not close " + name + " sessions")

    def settle(self):
        for name in self.clients:
            self.finish(name)
        self.wait("publication of closed sessions", lambda: all(all(s["published"] or s.get("discarded") for s in self.state(name)["manifest"]["sessions"].values()) for name in self.clients), 150)
        assert not self.conflicts("alice") and not self.conflicts("bob")

    def offline(self, name, value):
        # Failure injection affects this test client's transport only, never Wi-Fi,
        # global Git settings, or the user's configured wiki.
        url = str(self.root / "unavailable-remote.git") if value else self.remote
        self.git(name, "config", "remote.origin.url", url)
        if value:
            self.wait(name + " transport failure active", lambda: "Git fetch" in (self.state(name).get("attention") or ""), 45)

    def seed(self, relative, data):
        self.write("alice", relative, data)
        self.wait("seed draft received", lambda: self.received("bob", relative, data))
        self.settle()
        self.wait("seed main received", lambda: self.state("alice")["main"] == self.state("bob")["main"])

    def live_drafts(self):
        path = self.path("live.md")
        existing = self.state("alice").get("intent")
        if existing:
            assert existing["paths"] == [path], "Another intent scope is active"
            intent = existing["id"]
        else:
            intent = json.loads(self.mesh("alice", "intent", "begin", "--purpose", "Exercise live GitHub draft synchronization before session finalization", "--paths", path))["intent_id"]
        self.write("alice", path, b"Live draft, first save.\n")
        seconds = self.wait("live draft on Bob", lambda: self.received("bob", path, b"Live draft, first save.\n"))
        session = self.active("alice")
        assert session is not None and not session["published"]
        assert self.active("bob") is None, "Receiving a draft became Bob's local session"
        self.write("alice", path, b"Live draft, second save.\n")
        self.wait("updated draft on Bob", lambda: self.received("bob", path, b"Live draft, second save.\n"))
        assert self.active("alice")["id"] == session["id"]
        self.mesh("alice", "intent", "end", intent)
        self.settle()
        history = self.git("alice", "log", "--format=%B", self.state("alice")["main"]).decode()
        assert "Exercise live GitHub draft synchronization" in history
        return f"Draft reached Bob in {seconds}s, with one stable editing session and no incoming-edit attribution. Intent survived finalization."

    def finish_then_edit(self):
        first = self.path("finish-first.md")
        second = self.path("finish-next.md")
        self.write("alice", first, b"Frozen first session\n")
        self.wait("first edit captured", lambda: self.captured("alice", first, b"Frozen first session\n"))
        frozen = self.finish("alice")
        self.write("alice", second, b"Editing continues immediately\n")
        self.wait("next edit captured", lambda: self.captured("alice", second, b"Editing continues immediately\n"))
        state = self.state("alice")
        active = self.active("alice")
        assert active and active["id"] != frozen
        assert second not in state["manifest"]["sessions"][frozen]["changes"]
        self.settle()
        return "Immediate follow-up edits entered a separate session; the frozen session did not absorb them."

    def offline_restart(self):
        path = self.path("offline.md")
        data = b"Saved offline and recovered after terminating Mesh.\n"
        self.offline("bob", True)
        self.wait("Bob offline status", lambda: self.state("bob").get("attention") is not None)
        self.write("bob", path, data)
        self.wait("offline durable capture", lambda: self.captured("bob", path, data), 30)
        self.stop("bob")
        assert self.captured("bob", path, data)
        self.start("bob")
        self.wait("restart recovered saved edit", lambda: self.captured("bob", path, data), 30)
        self.offline("bob", False)
        self.wait("offline edit delivered after reconnect", lambda: self.received("alice", path, data))
        self.settle()
        return "Per-client transport failure did not block capture. Durable edits survived process termination/restart and synchronized after reconnect."

    def compatible_edits(self):
        path = self.path("compatible.md")
        base = b"one\ntwo\nthree\nfour\nfive\n"
        self.seed(path, base)
        self.offline("alice", True)
        self.offline("bob", True)
        self.write("alice", path, b"ONE\ntwo\nthree\nfour\nfive\n")
        self.write("bob", path, b"one\ntwo\nthree\nfour\nFIVE\n")
        self.wait("independent edits captured", lambda: self.captured("alice", path, b"ONE\ntwo\nthree\nfour\nfive\n") and self.captured("bob", path, b"one\ntwo\nthree\nfour\nFIVE\n"), 30)
        self.offline("alice", False)
        self.offline("bob", False)
        expected = b"ONE\ntwo\nthree\nfour\nFIVE\n"
        self.wait("compatible edits merged", lambda: all(self.received(n, path, expected) for n in self.clients))
        assert not self.conflicts("alice") and not self.conflicts("bob")
        self.settle()
        assert all(self.received(n, path, expected) for n in self.clients)
        return "Independent changes to the first and last lines merged and remained intact after both sessions published."

    def conflict_case(self, binary):
        kind = "binary" if binary else "text"
        path = self.path("conflict-" + kind + (".bin" if binary else ".md"))
        base, left, right = (b"\x00original\xff", b"\x00alice\xff", b"\x00bob\xff") if binary else (b"Retention: 30 days\n", b"Retention: 60 days\n", b"Retention: 90 days\n")
        self.seed(path, base)
        self.offline("alice", True)
        self.offline("bob", True)
        self.write("alice", path, left)
        self.write("bob", path, right)
        self.wait(kind + " edits captured", lambda: self.captured("alice", path, left) and self.captured("bob", path, right), 30)
        self.offline("alice", False)
        self.offline("bob", False)
        self.wait(kind + " conflict visible", lambda: any(c["path"] == path for c in self.conflicts("alice")) and any(c["path"] == path for c in self.conflicts("bob")))
        conflict = next(c for c in self.conflicts("alice") if c["path"] == path)
        assert conflict["binary"] == binary
        output = self.root / "reviews" / (kind + "-" + uuid.uuid4().hex[:8])
        self.mesh("alice", "conflicts", "export", conflict["id"], "--output", str(output))
        assert (output / "base").read_bytes() == base
        assert {(output / "accepted").read_bytes(), (output / "incoming").read_bytes()} == {left, right}
        unrelated = self.path("unrelated-" + kind + ".md")
        unrelated_data = ("Unrelated files keep synchronizing: " + uuid.uuid4().hex + "\n").encode()
        self.write("alice", unrelated, unrelated_data)
        self.wait("unrelated file during " + kind + " conflict", lambda: self.received("bob", unrelated, unrelated_data))
        choice = "accepted" if binary else "incoming"
        selected = (output / choice).read_bytes()
        self.mesh("alice", "conflicts", "resolve", conflict["id"], "--choose", choice)
        self.wait(kind + " resolution delivered", lambda: all(self.received(n, path, selected) and not any(c["path"] == path for c in self.conflicts(n)) for n in self.clients))
        # Explicitly exercise the publication order that exposed a regression:
        # the incoming contributor settles before the accepted contributor.
        owner_device = self.state("alice")["manifest"]["sessions"][conflict["session"]]["device"]
        owner = next(name for name in self.clients if json.loads((self.clients[name] / ".mesh/config.json").read_text())["device"] == owner_device)
        self.finish(owner)
        self.wait(kind + " incoming session settled first", lambda: self.state(owner)["manifest"]["sessions"][conflict["session"]]["published"] or self.state(owner)["manifest"]["sessions"][conflict["session"]].get("discarded"), 150)
        self.settle()
        assert all(self.received(n, path, selected) for n in self.clients), "Publication changed the selected resolution"
        return f"Exact base and competing versions exported intact; unrelated drafts continued; '{choice}' selection survived incoming-contributor-first publication."

    def renames_deletes(self):
        old, new = self.path("space café.md"), self.path("renamed résumé.md")
        deleted = self.path("delete-me.md")
        self.seed(old, b"Unicode filename rename\n")
        self.seed(deleted, b"Temporary test document\n")
        (self.clients["alice"] / old).rename(self.clients["alice"] / new)
        (self.clients["alice"] / deleted).unlink()
        self.wait("rename and deletion delivered", lambda: self.received("bob", new, b"Unicode filename rename\n") and not (self.clients["bob"] / old).exists() and not (self.clients["bob"] / deleted).exists())
        self.settle()
        return "Rename with spaces/Unicode and deletion propagated with content preserved, then finalized."

    def ignored_content(self):
        env = self.path(".env")
        self.write("alice", env, b"MESH_TEST_FAKE_SECRET=not-a-real-secret\n")
        marker = self.path("ignore-marker.md")
        self.write("alice", marker, b"Public marker\n")
        self.wait("public marker delivered", lambda: self.received("bob", marker, b"Public marker\n"))
        assert env not in self.state("alice")["observed"]
        assert not (self.clients["bob"] / env).exists()
        self.settle()
        names = self.git("alice", "ls-tree", "-r", "--name-only", self.state("alice")["main"]).decode()
        assert env not in names
        return "A nested .env containing only a synthetic value was excluded from capture, peer delivery, and main history."

    def real_idle_boundary(self):
        assert json.loads((self.clients["alice"] / ".mesh/config.json").read_text())["idle_seconds"] == 300
        first, second = self.path("idle-first.md"), self.path("idle-second.md")
        self.write("alice", first, b"First file in automatic session\n")
        self.wait("first grouped edit captured", lambda: self.captured("alice", first, b"First file in automatic session\n"))
        id = self.active("alice")["id"]
        self.write("alice", second, b"Second file in automatic session\n")
        self.wait("second grouped edit captured", lambda: self.captured("alice", second, b"Second file in automatic session\n"))
        session = self.active("alice")
        assert session["id"] == id and {first, second} <= session["changes"].keys()
        last_edit = session["last_edit"]
        self.wait("real five-minute inactivity boundary", lambda: self.state("alice")["manifest"]["sessions"][id]["published"], 350)
        finished = self.state("alice")["manifest"]["sessions"][id]
        assert finished["closed"] and time.time() - last_edit >= 300
        log = self.git("alice", "log", "--format=%B", self.state("alice")["main"]).decode()
        assert log.count("Mesh-Session: " + id) == 1
        return f"Two files shared session {id}; automatic publication followed the real 300-second inactivity timer, with one final session commit."

    def report_results(self):
        self.report.parent.mkdir(parents=True, exist_ok=True)
        day = self.run_id.split('-')[1]
        date = f"{day[:4]}-{day[4:6]}-{day[6:8]}"
        lines = ["# GitHub workflow test report", "", f"Date: {date}", "", f"Remote: {self.remote}", f"Run: `{self.run_id}`", f"Evidence directory: `{self.root}`", f"Current build: `{self.build}`, SHA-256 `{self.binary_hash}`", "", "Scope: two isolated Windows clients on one machine, real GitHub transport, synthetic text/binary data, local commit messages. Alice and Bob use distinct Mesh device/author identities and the machine's existing GitHub authorization. No wiki files or live AI provider were used.", "", "Attempts are listed chronologically; a later passing rerun supersedes an earlier failure of the same case. Initial failures remain as evidence of defects found during testing.", "", "| Case | Result | Seconds | Build hash | Evidence |", "|---|---|---:|---|---|"]
        for result in self.results:
            lines.append(f"| {result['case']} | {result['status']} | {result['seconds']} | {result.get('binary_hash', 'initial')[:12]} | {result['detail'].replace('|', '/').replace(chr(10), ' ')} |")
        lines += ["", "Process logs, SQLite journals, Git objects, and exported conflict inputs remain in the evidence directory. All owned runner processes are stopped and test transport overrides are restored at completion. Existing remote history is retained.", "", "This run does not validate separate physical machines or GitHub accounts, macOS/Linux hosts, tray UI, guided setup, cloud authorization, or live AI output quality.", ""]
        self.report.write_text("\n".join(lines), encoding="utf-8")
        self.save_context()

    def run(self, start_case, only_cases):
        cases = [("live_drafts", self.live_drafts), ("finish_then_edit", self.finish_then_edit), ("offline_restart", self.offline_restart), ("compatible_edits", self.compatible_edits), ("text_conflict", lambda: self.conflict_case(False)), ("binary_conflict", lambda: self.conflict_case(True)), ("renames_deletes", self.renames_deletes), ("ignored_content", self.ignored_content), ("real_idle_boundary", self.real_idle_boundary)]
        started = start_case is None
        try:
            for name in self.clients:
                self.start(name)
            for name, function in cases:
                started |= name == start_case
                if not started:
                    continue
                if only_cases and name not in only_cases:
                    continue
                print("START", name, flush=True)
                before = time.monotonic()
                try:
                    detail = function()
                    result = {"case": name, "status": "PASS", "seconds": round(time.monotonic() - before, 2), "detail": detail, "binary_hash": self.binary_hash}
                except Exception as error:
                    result = {"case": name, "status": "FAIL", "seconds": round(time.monotonic() - before, 2), "detail": str(error), "binary_hash": self.binary_hash}
                    self.results.append(result)
                    print("FAIL", name, str(error), flush=True)
                    self.report_results()
                    raise
                self.results.append(result)
                print("PASS", name, result["seconds"], detail, flush=True)
                self.report_results()
        finally:
            for name in self.clients:
                self.stop(name)
                self.offline(name, False)
            self.report_results()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--remote", required=True)
    parser.add_argument("--exe", type=Path, default=Path("target/release/dispatch-mesh.exe"))
    parser.add_argument("--report", type=Path, default=Path("reports") / f"github-workflow-test-{datetime.now(timezone.utc).date()}.md")
    parser.add_argument("--resume", type=Path)
    parser.add_argument("--from-case")
    parser.add_argument("--only-case", action="append", choices=["live_drafts", "finish_then_edit", "offline_restart", "compatible_edits", "text_conflict", "binary_conflict", "renames_deletes", "ignored_content", "real_idle_boundary"])
    args = parser.parse_args()
    LiveTest(args).run(args.from_case, args.only_case)
