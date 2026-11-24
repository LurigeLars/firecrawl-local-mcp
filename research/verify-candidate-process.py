"""Smoke actual isolated Rust candidate process; no external network/keys."""
import json
import os
import pathlib
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

BASE="http://127.0.0.1:3002"
TOKEN="synthetic-ci-token-not-a-real-secret-0123456789"
BIN="/usr/local/bin/candidate-main"

def call(path, *, payload=None, token=TOKEN, extra=None):
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
        with opener.open(req,timeout=2) as res:
            return res.status,res.read(131072)
    except urllib.error.HTTPError as e:
        return e.code,e.read(131072)

with tempfile.TemporaryDirectory(prefix="fc-candidate-") as td:
    token_path=pathlib.Path(td)/"token"
    env={**os.environ,"CANDIDATE_TOKEN_FILE":str(token_path)}
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
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=5)
