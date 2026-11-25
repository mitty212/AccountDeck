"""Real browser persistence/isolation, with no third-party login or account data.

Build: cargo build --example persistence_check
Run: python qa/check_persistence.py [--exe PATH] [--output DIRECTORY]
The same loopback origin stays alive while separate seed and verify EXEs run.
"""

import argparse
from http.cookies import SimpleCookie
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import secrets
import subprocess
import threading
import time
from urllib.parse import parse_qs, urlsplit


def wait_for_clean_browser_shutdown(data, output):
    """Observe only engine processes owned by these disposable test profiles.

    The script returns counts, never process command lines. Nothing is killed.
    A fresh harness process alone would not prove a fresh WebView2 engine.
    """
    script = output / "check-browser-shutdown.ps1"
    script.write_text(r'''param([Parameter(Mandatory=$true)][string]$TestRoot)
$ErrorActionPreference = 'Stop'
$root = [IO.Path]::GetFullPath($TestRoot).TrimEnd('\')
$deadline = [DateTime]::UtcNow.AddSeconds(10)
$pattern = '(?i)(?:^|\s)(?:"--user-data-dir=(?<whole>[^"]+)"|--user-data-dir(?:=|\s+)"(?<quoted>[^"]+)"|--user-data-dir(?:=|\s+)(?<bare>[^\s"]+))'
do {
    $count = 0
    $processes = @(Get-CimInstance Win32_Process -Filter "Name = 'msedgewebview2.exe'" -OperationTimeoutSec 2)
    foreach ($process in $processes) {
        if ([string]::IsNullOrEmpty($process.CommandLine)) { continue }
        $match = [regex]::Match($process.CommandLine, $pattern)
        if (!$match.Success) { continue }
        $candidate = $match.Groups['whole'].Value
        if (!$candidate) { $candidate = $match.Groups['quoted'].Value }
        if (!$candidate) { $candidate = $match.Groups['bare'].Value }
        try { $candidate = [IO.Path]::GetFullPath($candidate).TrimEnd('\') }
        catch { continue }
        if ($candidate.Equals($root, [StringComparison]::OrdinalIgnoreCase) -or
            $candidate.StartsWith($root + '\', [StringComparison]::OrdinalIgnoreCase)) { $count += 1 }
    }
    if ($count -eq 0) { break }
    if ([DateTime]::UtcNow -ge $deadline) { break }
    Start-Sleep -Milliseconds 200
} while ($true)
[pscustomobject]@{clean_browser_shutdown=($count -eq 0); remaining_browser_processes=$count} | ConvertTo-Json -Compress
''', encoding="utf-8")
    try:
        process = subprocess.run(["powershell.exe", "-NoProfile", "-NonInteractive", "-File", str(script), "-TestRoot", str(data)], capture_output=True, text=True, timeout=15)
        if process.returncode:
            raise RuntimeError("Could not observe test browser shutdown through Win32_Process.")
        observed = json.loads(process.stdout)
        if not isinstance(observed.get("clean_browser_shutdown"), bool) or not isinstance(observed.get("remaining_browser_processes"), int):
            raise RuntimeError("Invalid browser shutdown observation.")
        return observed
    finally:
        script.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    root = Path(__file__).resolve().parents[1]
    parser.add_argument("--exe", type=Path, default=root / "target/debug/examples/persistence_check.exe")
    parser.add_argument("--output", type=Path, default=root / "qa/results" / ("persistence-" + time.strftime("%Y%m%d-%H%M%S")))
    args = parser.parse_args()
    executable = args.exe.resolve()
    if not executable.is_file():
        raise SystemExit(f"Missing harness: {executable}. Build cargo build --example persistence_check first.")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    data = output / "profiles"
    if data.exists():
        raise SystemExit("Use a new output directory: the check requires fresh browser profiles.")
    tokens = {profile: f"{profile}-{secrets.token_hex(16)}" for profile in ("A", "B")}
    requests = []
    request_lock = threading.Lock()

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            request = urlsplit(self.path)
            if request.path not in ("/seed", "/verify"):
                self.send_error(404)
                return
            profile = parse_qs(request.query).get("profile", [""])[0]
            if profile not in ("A", "B", "C") or request.path == "/seed" and profile == "C":
                self.send_error(400)
                return
            received = SimpleCookie()
            received.load(self.headers.get("Cookie", ""))
            cookies = {name: item.value for name, item in received.items()}
            token = tokens.get(profile)
            expected = {} if profile == "C" or request.path == "/seed" else {"sid": token, "remember": token}
            cookies_ok = cookies == expected
            with request_lock:
                # Record booleans, never session values.
                requests.append({"phase": request.path[1:], "profile": profile, "cookies_ok": cookies_ok, "cookie_names": sorted(cookies)})
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Cache-Control", "no-store")
            if request.path == "/seed":
                self.send_header("Set-Cookie", f"sid={token}; Path=/; HttpOnly; SameSite=Lax")
                self.send_header("Set-Cookie", f"remember={token}; Path=/; Max-Age=3600; SameSite=Lax")
            self.end_headers()
            parameters = json.dumps({"profile": profile, "token": token, "seed": request.path == "/seed", "cookiesOk": cookies_ok})
            html = """<!doctype html><meta charset="utf-8"><title>WORKING</title>
<script>
(async () => {
  const p = PARAMETERS;
  const check = (condition, reason) => { if (!condition) throw new Error(reason); };
  check(p.cookiesOk, 'server-cookie-isolation');
  const db = await new Promise((resolve, reject) => {
    const open = indexedDB.open('accountdeck-isolation-v1', 1);
    open.onupgradeneeded = () => open.result.createObjectStore('values');
    open.onsuccess = () => resolve(open.result);
    open.onerror = () => reject(new Error('idb-open'));
  });
  const read = () => new Promise((resolve, reject) => {
    const tx = db.transaction('values', 'readonly');
    const req = tx.objectStore('values').get('profile');
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(new Error('idb-read'));
  });
  if (p.seed) {
    check(localStorage.getItem('profile') === null && await read() === undefined, 'seed-not-empty');
    localStorage.setItem('profile', p.token);
    await new Promise((resolve, reject) => {
      const tx = db.transaction('values', 'readwrite');
      tx.objectStore('values').put(p.token, 'profile');
      tx.oncomplete = resolve;
      tx.onabort = () => reject(new Error('idb-write-aborted'));
      tx.onerror = () => reject(new Error('idb-write'));
    });
    check(document.cookie.includes('remember=' + p.token), 'persistent-cookie-client');
  } else if (p.profile === 'C') {
    check(localStorage.getItem('profile') === null && await read() === undefined, 'fresh-profile-has-data');
    check(document.cookie === '', 'fresh-profile-has-cookie');
  } else {
    check(localStorage.getItem('profile') === p.token, 'localstorage-restart');
    check(await read() === p.token, 'indexeddb-restart');
    check(document.cookie.includes('remember=' + p.token), 'persistent-cookie-client');
  }
  check(!document.cookie.split(';').some(value => value.trim().startsWith('sid=')), 'httponly-exposed');
  db.close();
  document.title = (p.seed ? 'READY:' : 'PASS:') + p.profile;
})().catch(error => { document.title = 'FAIL:' + error.message; });
</script>""".replace("PARAMETERS", parameters)
            self.wfile.write(html.encode("utf-8"))

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    origin = f"http://127.0.0.1:{server.server_port}/"
    results = []
    try:
        for phase in ("seed", "verify"):
            result_path = output / f"{phase}.json"
            process = subprocess.run([str(executable), phase, str(data), origin, str(result_path)], capture_output=True, text=True, timeout=45)
            (output / f"{phase}.log").write_text(process.stdout + process.stderr, encoding="utf-8")
            if not result_path.is_file():
                raise RuntimeError(f"{phase} produced no result (exit {process.returncode}). See {phase}.log")
            result = json.loads(result_path.read_text(encoding="utf-8"))
            results.append(result)
            if process.returncode or not result["passed"]:
                raise RuntimeError(f"{phase} failed: {result.get('error')} (exit {process.returncode})")
            shutdown = wait_for_clean_browser_shutdown(data, output)
            result.update(shutdown)
            result_path.write_text(json.dumps(result, indent=2), encoding="utf-8")
            if not shutdown["clean_browser_shutdown"]:
                raise RuntimeError(f"{phase} left {shutdown['remaining_browser_processes']} test WebView2 process(es) running after 10 seconds; a fresh engine restart is not established.")
            if phase == "seed":
                for profile in ("A", "B"):
                    encrypted = (data / profile / "session.bin").read_bytes()
                    if not encrypted.startswith(b"AccountDeck cookies v1\0"):
                        raise RuntimeError("Missing encrypted-session format marker")
                    if any(token.encode() in encrypted or token.encode("utf-16-le") in encrypted for token in tokens.values()):
                        raise RuntimeError("Cookie snapshot contains a plaintext test session")
        with request_lock:
            observed = list(requests)
        for phase, profiles in (("seed", ("A", "B")), ("verify", ("A", "B", "C"))):
            for profile in profiles:
                matching = [item for item in observed if item["phase"] == phase and item["profile"] == profile]
                if not matching or not all(item["cookies_ok"] for item in matching):
                    raise RuntimeError(f"Missing or incorrect server-observed cookies: {phase}/{profile}")
        report = {"passed": True, "clean_browser_shutdown": all(result["clean_browser_shutdown"] for result in results), "checks": ["separate-process-restart", "fresh-webview2-engine-after-seed", "clean-webview2-shutdown-after-verify", "per-profile-cookie-isolation", "httponly-session-cookie-restored", "persistent-cookie-restored", "localstorage-restored", "indexeddb-restored", "fresh-profile-C-empty", "dpapi-snapshot-no-plaintext-token"], "phases": results, "requests": observed}
        (output / "summary.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
        print(f"PASS: isolated cookies, HttpOnly session cookies, localStorage and IndexedDB survived restart. {output / 'summary.json'}")
    except Exception as error:
        (output / "summary.json").write_text(json.dumps({"passed": False, "error": str(error), "phases": results, "requests": requests}, indent=2), encoding="utf-8")
        raise
    finally:
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    main()
