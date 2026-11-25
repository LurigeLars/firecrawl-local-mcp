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

def settled_job(route,payload,seconds=65):
    status,raw=call(route,payload=payload)
    assert status==202,(route,status)
    job_id=json.loads(raw)["id"]
    deadline=time.monotonic()+seconds
    while time.monotonic()<deadline:
        status,raw=call("/research/batch/"+job_id)
        assert status==200,(route,status)
        result=json.loads(raw)["data"]
        if result["status"]=="completed":
            return job_id,result
        time.sleep(.2)
    raise AssertionError("job did not settle within bound")

def verify_live_jobs():
    # Fixed official public sources only. No retries or robots override.
    cases={
        "https://www.iana.org/help/example-domains":["Example Domains","RFC 2606","RFC 6761"],
        "https://www.cftc.gov/sites/default/files/files/dea/cotarchives/2026/futures/petroleum_sf090826.htm":
            ["September 8, 2026","Open Interest","1,939,911"]
    }
    started=time.monotonic()
    batch_id,batch=settled_job("/research/batch",{"urls":list(cases)})
    assert batch["total"]==2 and batch["completed"]==2 and batch["failed"]==0,batch["status"]
    assert {r["key"] for r in batch["results"]}==set(cases)
    for row in batch["results"]:
        assert row["state"]=="done"
        assert row["data"]["source"]=="local"
        text=" ".join(row["data"]["markdown"].split()).lower()
        assert all(m.lower() in text for m in cases[row["key"]])
    assert batch["next_after"] is None
    print("LIVE_BATCH_PASS",json.dumps({"pages":2,"markers":6,"elapsed_ms":round((time.monotonic()-started)*1000)}))
    started=time.monotonic()
    seed="https://www.iana.org/help"
    crawl_id,crawl=settled_job("/research/crawl",{"url":seed,"limit":2,"maxDiscoveryDepth":1},115)
    assert crawl["total"]==2 and crawl["completed"]==2 and crawl["failed"]==0,(
        "bounded live crawl not verified",crawl["total"],crawl["completed"],crawl["failed"])
    assert crawl["crawl"]["page_attempts"]==2 and crawl["crawl"]["page_attempt_limit"]==6
    assert crawl["crawl"]["site_coverage_verified"] is False
    assert crawl["next_after"] is None
    for row in crawl["results"]:
        assert row["state"]=="done" and row["data"]["source"]=="local"
        assert row["data"]["markdown"].strip()
        assert row["key"]==seed or row["key"].startswith(seed+"/")
    print("LIVE_CRAWL_PASS",json.dumps({"pages":2,"attempts":2,"site_coverage_verified":False,
        "elapsed_ms":round((time.monotonic()-started)*1000)}))
    return {batch_id:batch,crawl_id:crawl}

with tempfile.TemporaryDirectory(prefix="fc-candidate-") as td:
    token_path=pathlib.Path(td)/"token"
    env={**os.environ,"CANDIDATE_TOKEN_FILE":str(token_path)}
    saved_jobs={}
    env.pop("CANDIDATE_PDF_ENABLED",None)
    if "--batch" in sys.argv or "--live-jobs" in sys.argv:
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
            assert call("/research/pdf",payload={"url":"https://www.iana.org/report.pdf"})[0]==503
            assert call("/research/pdf",payload={"url":"https://127.0.0.1/x.pdf"})[0]==403
            print("CANDIDATE_PROCESS_SMOKE_PASS: authenticated loopback, unsupported options and private destinations rejected")
            if "--live" in sys.argv:
                verify_live(proc.pid)
                # Actual default MCP request envelope, without research-only
                # maxAge/storeInCache/timeout fields.
                status,raw=call("/v2/scrape",payload={"url":"https://www.iana.org/help/example-domains",
                    "formats":["markdown"],"origin":"mcp-candidate-contract@3.40.0"},timeout=35)
                assert status==200
                result=json.loads(raw)
                assert "RFC 2606" in result["data"]["markdown"]
                assert result["research"]["cache_used"] is False
                status,raw=call("/v2/scrape",payload={"url":"https://www.iana.org/help/example-domains",
                    "formats":["html","links"],"onlyMainContent":False,"origin":"mcp-candidate-contract@3.40.0"},timeout=35)
                assert status==200
                data=json.loads(raw)["data"]
                assert "RFC 2606" in data["html"] and len(data["links"])>0
                assert "markdown" not in data
                print("MCP_SCRAPE_ENVELOPE_LIVE_PASS: minimal request and HTML/link formats")
            if "--batch" in sys.argv or "--live-jobs" in sys.argv:
                assert json.loads(call("/health")[1])["jobStore"]=="ready"
                assert call("/research/crawl",payload={"url":"https://127.0.0.1/","limit":2,"maxDiscoveryDepth":1})[0]==403
                assert call("/research/crawl",payload={"url":"https://www.iana.org/help","limit":2,"maxDiscoveryDepth":1,"ignoreRobotsTxt":True})[0]==400
                assert call("/research/crawl",payload={"url":"https://www.iana.org/help","limit":101,"maxDiscoveryDepth":1})[0]==400
            if "--live-jobs" in sys.argv:
                saved_jobs.update(verify_live_jobs())
            if "--batch" in sys.argv:
                assert call("/research/batch",payload={"urls":["https://127.0.0.1/"]})[0]==403
                # No egress in this test: a real guarded request must settle as
                # an explicit per-item failure, not an invented document.
                batch_id,saved_batch=settled_job("/research/batch",{"urls":["https://93.184.216.34/report"]},30)
                saved_jobs[batch_id]=saved_batch
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

    if saved_jobs:
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
                for job_id,saved in saved_jobs.items():
                    status,body=call("/research/batch/"+job_id)
                    assert status==200
                    assert json.loads(body)["data"]==saved
                print("DURABLE_JOB_RESTART_PASS",json.dumps({"jobs":len(saved_jobs),"live":"--live-jobs" in sys.argv}))
                if "--batch" in sys.argv:
                    print("DURABLE_BATCH_PROCESS_PASS: real worker settled failure; identical status and result after restart")
            finally:
                proc.terminate()
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait(timeout=5)
