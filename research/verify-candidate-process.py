"""Smoke actual isolated Rust candidate process; no external network/keys."""
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

BASE="http://127.0.0.1:3002"
TOKEN="synthetic-ci-token-not-a-real-secret-0123456789"
BIN="/usr/local/bin/candidate-main"

def call(path, *, payload=None, token=TOKEN, extra=None, timeout=2):
    headers = dict(extra or {})
    if token is not None:
        headers["Authorization"]="Bearer "+token
    if payload is not None:
        headers["Content-Type"]="application/json"
    req=urllib.request.Request(BASE+path,
        data=json.dumps(payload).encode() if payload is not None else None,
        headers=headers)
    # Ignore environment HTTP proxies, and never follow external redirects.
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *args, **kwargs):
            return None
    opener=urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    try:
        with opener.open(req,timeout=timeout) as res:
            return res.status,res.read(2*1024*1024+1)
    except urllib.error.HTTPError as e:
        return e.code,e.read(131072)

def memory_kib(pid, field):
    for line in pathlib.Path(f"/proc/{pid}/status").read_text().splitlines():
        if line.startswith(field+":"):
            return int(line.split()[1])
    return None

def verify_live(pid):
    # Fixed public official-source URLs, no user input, credentials, browser
    # sessions, retries or scraping of search results.
    cases=[
        ("IANA", "https://www.iana.org/help/example-domains",
            ["Example Domains","RFC 2606","RFC 6761"]),
        ("CFTC_ARCHIVE","https://www.cftc.gov/sites/default/files/files/dea/cotarchives/2026/futures/petroleum_sf090826.htm",
            ["September 8, 2026","Open Interest","1,939,911"]),
    ]
    initial=memory_kib(pid,"VmRSS")
    all_pass=True
    for label,url,markers in cases:
        started=time.monotonic()
        verdict="INCONCLUSIVE"
        status=None
        matches=0
        chars=0
        try:
            status,raw=call("/v2/scrape",payload={"url":url,"formats":["markdown"],
                "onlyMainContent":True,"maxAge":0,"storeInCache":False,"timeout":25000},timeout=30)
            if status==200:
                data=json.loads(raw)["data"]
                markdown=" ".join(data["markdown"].split())
                matches=sum(m.lower() in markdown.lower() for m in markers)
                chars=len(markdown)
                verdict="PASS" if matches==len(markers) and data["source"]=="local" else "FAIL"
        except (OSError,ValueError,KeyError,TypeError,urllib.error.URLError):
            verdict="INCONCLUSIVE"
        print(json.dumps({"live_source":label,"verdict":verdict,"api_status":status,
            "markers":f"{matches}/{len(markers)}","markdown_chars":chars,
            "elapsed_ms":round((time.monotonic()-started)*1000),
            "process_rss_kib":memory_kib(pid,"VmRSS"),"process_peak_rss_kib":memory_kib(pid,"VmHWM"),
            "initial_process_rss_kib":initial}))
        all_pass &= verdict=="PASS"
    if not all_pass:
        raise SystemExit("LIVE_CANDIDATE_NOT_VERIFIED")

with tempfile.TemporaryDirectory(prefix="fc-candidate-") as td:
    token_path=pathlib.Path(td)/"token"
    env={**os.environ,"CANDIDATE_TOKEN_FILE":str(token_path)}
    batch_id=None
    saved_batch=None
    if "--batch" in sys.argv:
        env["CANDIDATE_JOB_DB_FILE"]=str(pathlib.Path(td)/"jobs.sqlite")
    bad=subprocess.run([BIN],env=env,capture_output=True,timeout=5)
    assert bad.returncode!=0,"must refuse missing token"
    token_path.write_text(TOKEN)
    token_path.chmod(0o600)
    with subprocess.Popen([BIN],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL) as proc:
        try:
            for _ in range(50):
                assert proc.poll() is None,"candidate exited"
                try:
                    status,body=call("/health")
                    if status==200:
                        assert json.loads(body)["research"] is True
                        break
                except (OSError,urllib.error.URLError):
                    pass
                time.sleep(.1)
            else:
                raise AssertionError("candidate health unavailable")
            assert call("/health",token=None)[0]==401
            assert call("/health",extra={"Host":"bad.example"})[0]==403
            static={"url":"https://127.0.0.1/","formats":["markdown"],"onlyMainContent":True,
                "maxAge":0,"storeInCache":False,"timeout":1000}
            assert call("/v2/scrape",payload=static)[0]==403
            assert call("/v2/scrape",payload={**static,"headers":{"Cookie":"synthetic"}})[0]==400
            assert call("/v2/search",payload={"query":"test","endpoint":"http://localhost"})[0]==400
            assert call("/research/render",payload={"url":"https://169.254.169.254/","onlyMainContent":True})[0]==403
            assert call("/v2/crawl",payload={})[0]==404
            print("CANDIDATE_PROCESS_SMOKE_PASS: authenticated loopback, unsupported options and private destinations rejected")
            if "--live" in sys.argv:
                verify_live(proc.pid)
            if "--batch" in sys.argv:
                assert json.loads(call("/health")[1])["jobStore"]=="ready"
                assert call("/research/batch",payload={"urls":["https://127.0.0.1/"]})[0]==403
                # No egress in this test: a real guarded request must settle as
                # an explicit per-item failure, not an invented document.
                status,body=call("/research/batch",payload={"urls":["https://93.184.216.34/report"]})
                assert status==202
                batch_id=json.loads(body)["id"]
                deadline=time.monotonic()+30
                while time.monotonic()<deadline:
                    status,body=call("/research/batch/"+batch_id)
                    assert status==200
                    saved_batch=json.loads(body)["data"]
                    if saved_batch["status"]=="completed":
                        break
                    time.sleep(.1)
                else:
                    raise AssertionError("durable worker did not settle")
                assert saved_batch["failed"]==1 and saved_batch["completed"]==0
                assert saved_batch["results"][0]["state"]=="failed"
                assert call("/research/batch/"+batch_id,token=None)[0]==401
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=5)

    if batch_id:
        # Reopen the same private DB in a new actual server process.
        with subprocess.Popen([BIN],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL) as proc:
            try:
                for _ in range(50):
                    assert proc.poll() is None,"restarted candidate exited"
                    try:
                        if call("/health")[0]==200:
                            break
                    except (OSError,urllib.error.URLError):
                        pass
                    time.sleep(.1)
                else:
                    raise AssertionError("restarted candidate health unavailable")
                status,body=call("/research/batch/"+batch_id)
                assert status==200
                assert json.loads(body)["data"]==saved_batch
                print("DURABLE_BATCH_PROCESS_PASS: real worker settled failure; identical status and result after restart")
            finally:
                proc.terminate()
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait(timeout=5)
