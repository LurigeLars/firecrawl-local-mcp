"""One-shot actual browser sidecar smoke, fixed public source and own denial fixture.

No proxy credentials, production volumes, arbitrary endpoints or external retry.
Explicit waitFor path only. Does not prove automatic fallback for dynamic sites.
"""
import json
import time
import urllib.error
import urllib.request

CANDIDATE = "http://127.0.0.1:3002"
BROWSER = "http://playwright-service:3000"
ROUTER = "http://playwright-router:3000"
TOKEN = "synthetic-ci-token-not-a-real-secret-0123456789"
SOURCE = "https://www.iana.org/help/example-domains"

class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None

opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

def request(url, *, payload=None, auth=False, timeout=5):
    headers = {}
    if auth:
        headers["Authorization"] = "Bearer " + TOKEN
    raw = None
    if payload is not None:
        raw = json.dumps(payload).encode()
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=raw, headers=headers)
    try:
        with opener.open(req, timeout=timeout) as res:
            raw = res.read(2 * 1024 * 1024 + 1)
            assert len(raw) <= 2 * 1024 * 1024
            return res.status, json.loads(raw), res.headers
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read(4096)), error.headers

for _ in range(30):
    try:
        if (request(CANDIDATE + "/health", auth=True)[0] == 200
                and request(BROWSER + "/health", timeout=10)[0] == 200):
            break
    except (OSError, ValueError):
        pass
    time.sleep(1)
else:
    raise AssertionError("candidate/browser unavailable")

# Exercise the actual pinned browser's guard behind the actual router.
# This address is the disposable browser container's own loopback.
status, denied, headers = request(ROUTER + "/scrape", payload={
    "url": "http://127.0.0.1:3000/health",
    "timeout": 1000, "wait_after_load": 0, "skip_tls_verification": False,
})
assert status == 200 and denied["pageStatusCode"] == 403
assert denied["pageError"].startswith("Blocked insecure target URL ")
assert headers.get("x-firecrawl-public-proxy") == "direct"
status, _, _ = request(CANDIDATE + "/v2/scrape", auth=True, payload={
    "url": "http://127.0.0.1:3000/health", "formats": ["markdown"], "waitFor": 100,
})
assert status == 403
print("BROWSER_SIDECAR_POLICY_PASS")

started = time.monotonic()
status, result, _ = request(CANDIDATE + "/v2/scrape", auth=True, timeout=65, payload={
    "url": SOURCE, "formats": ["markdown", "links"], "onlyMainContent": True,
    "waitFor": 100, "timeout": 60000, "maxAge": 0, "storeInCache": False,
})
if status != 200:
    print("BROWSER_SIDECAR_LIVE_FAIL", json.dumps({"status": status, "code": result.get("code")}))
    raise SystemExit(1)
doc = result["data"]
assert result["success"] is True
assert result["research"]["router_path"] == "direct"
assert doc["metadata"]["statusCode"] == 200
assert doc["metadata"]["sourceURL"] == SOURCE
assert doc["researchSourceVerified"] is False
assert "rawHtml" not in doc
assert isinstance(doc["links"], list) and doc["links"]
text = " ".join(doc["markdown"].split()).lower()
assert all(marker.lower() in text for marker in ("Example Domains", "RFC 2606", "RFC 6761"))
print("BROWSER_SIDECAR_LIVE_PASS", json.dumps({
    "markers": 3, "route": "direct", "source_verified": False,
    "elapsed_ms": round((time.monotonic() - started) * 1000),
}))
