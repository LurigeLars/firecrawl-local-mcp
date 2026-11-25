"""Opt-in release sample: fixed public sources, max ten calls, no credentials.
This is candidate-only evidence, not a full production or legacy comparison.
"""
import concurrent.futures
import hashlib
import json
import os
import pathlib
import statistics
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

BASE = "http://127.0.0.1:3002"
TOKEN = "synthetic-release-fixture-not-a-secret-0123456789"
SOURCES = (
    ("IANA", "https://www.iana.org/help/example-domains",
     ("Example Domains", "RFC 2606", "RFC 6761")),
    ("CFTC", "https://www.cftc.gov/sites/default/files/files/dea/cotarchives/2026/futures/petroleum_sf090826.htm",
     ("September 8, 2026", "Open Interest", "1,939,911")),
)
CAP = 2 * 1024 * 1024

class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None

def request(path, payload=None, timeout=35):
    data = json.dumps(payload).encode() if payload is not None else None
    req = urllib.request.Request(BASE + path, data=data,
        headers={"Authorization": "Bearer " + TOKEN, "Content-Type": "application/json"})
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    with opener.open(req, timeout=timeout) as res:
        raw = res.read(CAP + 1)
        if res.status != 200 or len(raw) > CAP:
            raise ValueError("unexpected bounded response")
        return json.loads(raw)

def metric(name):
    try:
        return int(pathlib.Path("/sys/fs/cgroup", name).read_text().strip())
    except (OSError, ValueError):
        return None

def cpu_us():
    try:
        values = dict(row.split() for row in pathlib.Path("/sys/fs/cgroup/cpu.stat").read_text().splitlines())
        return int(values["usage_usec"])
    except (OSError, ValueError, KeyError):
        return None

def sample(case, mode):
    label, url, markers = case
    start = time.perf_counter()
    body = request("/v2/scrape", {"url":url, "formats":["markdown"],
        "onlyMainContent":True, "maxAge":0, "storeInCache":False, "timeout":25000})
    elapsed = round((time.perf_counter() - start) * 1000, 1)
    doc = body["data"]
    md = doc["markdown"]
    normalized = " ".join(md.split()).lower()
    if (body["success"] is not True or doc["source"] != "local" or
            doc["metadata"]["statusCode"] != 200 or
            not all(x.lower() in normalized for x in markers)):
        raise ValueError("source markers missing: " + label)
    return {"source":label, "mode":mode, "elapsed_ms":elapsed, "markers":len(markers),
            "markdown_bytes":len(md.encode()), "sha256":hashlib.sha256(md.encode()).hexdigest()}

def main():
    with tempfile.TemporaryDirectory(prefix="fc-release-") as td:
        token = pathlib.Path(td, "token")
        token.write_text(TOKEN)
        token.chmod(0o600)
        env = {"PATH":os.environ["PATH"], "HOME":td, "CANDIDATE_TOKEN_FILE":str(token),
               "CANDIDATE_JOB_DB_FILE":str(pathlib.Path(td,"jobs.sqlite"))}
        proc = subprocess.Popen(["/usr/local/bin/candidate-main"], env=env,
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            for _ in range(50):
                if proc.poll() is not None:
                    raise RuntimeError("candidate exited")
                try:
                    if request("/health", timeout=1)["jobStore"] == "ready":
                        break
                except (OSError, urllib.error.URLError):
                    time.sleep(0.1)
            else:
                raise RuntimeError("candidate not ready")
            initial = metric("memory.current")
            start_cpu = cpu_us()
            started = time.perf_counter()
            rows = []
            for _ in range(3):
                for case in SOURCES:
                    rows.append(sample(case,"serial"))
                    time.sleep(1)
            for _ in range(2):
                with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                    rows.extend(pool.map(lambda c: sample(c,"concurrent_2"), SOURCES))
                time.sleep(1)
            end_cpu = cpu_us()
            grouped = {}
            for mode in ("serial","concurrent_2"):
                for label, _, _ in SOURCES:
                    times = [r["elapsed_ms"] for r in rows if r["mode"] == mode and r["source"] == label]
                    grouped[mode + ":" + label] = {"n":len(times),"median_ms":statistics.median(times),"max_ms":max(times)}
            print(json.dumps({"verdict":"PASS", "profile":"release-thin-lto-codegen-units-8",
                "scope":"candidate-only, two fixed official HTML sources; not production parity",
                "requests":len(rows),"markers":sum(r["markers"] for r in rows),
                "wall_seconds_including_pacing":round(time.perf_counter()-started,2),
                "cgroup_initial_bytes":initial,"cgroup_peak_bytes":metric("memory.peak"),
                "cgroup_final_bytes":metric("memory.current"),
                "cgroup_cpu_usec":None if start_cpu is None or end_cpu is None else end_cpu-start_cpu,
                "groups":grouped,"samples":rows},sort_keys=True))
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=5)

if __name__ == "__main__":
    main()
