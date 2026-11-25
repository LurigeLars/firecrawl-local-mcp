"""Live PDF dispatch: pinned public synthetic test document and real isolated worker.

No user input or secrets. External source availability is reported separately
from the synthetic/offline correctness gate.
"""
import json
import time
import urllib.error
import urllib.request

CANDIDATE = "http://127.0.0.1:3002"
WORKER = "http://pdf-ocr:3000"
URL = "https://raw.githubusercontent.com/LurigeLars/firecrawl-local-mcp/b5a1bddc489c1d39fc2e45ef19ea77f7040c449b/research/fixtures/pdf-transport-native.pdf"
TOKEN = "synthetic-ci-token-not-a-real-secret-0123456789"

class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None

opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

def request(url, *, payload=None, auth=False, timeout=2):
    headers = {}
    if auth:
        headers["Authorization"] = "Bearer " + TOKEN
    raw = None
    if payload is not None:
        raw = json.dumps(payload).encode()
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=raw, headers=headers)
    with opener.open(req, timeout=timeout) as response:
        data = response.read(2 * 1024 * 1024 + 1)
        if response.status != 200 or len(data) > 2 * 1024 * 1024:
            raise AssertionError("unexpected response")
        return json.loads(data)

for _ in range(50):
    try:
        request(WORKER + "/health")
        request(CANDIDATE + "/health", auth=True)
        break
    except (OSError, urllib.error.URLError):
        time.sleep(.1)
else:
    raise AssertionError("candidate or isolated worker unavailable")

started = time.monotonic()
try:
    body = request(CANDIDATE + "/v2/scrape", auth=True, timeout=115, payload={
        "url": URL, "formats": ["markdown"], "parsers": ["pdf"],
        "maxAge": 0, "storeInCache": False, "timeout": 110000,
    })
except urllib.error.HTTPError as error:
    # Print only candidate's fixed failure envelope, never remote HTML or headers.
    try:
        result = json.loads(error.read(4096))
        code = result.get("code", result.get("error", "unknown"))
    except (ValueError, TypeError):
        code = "invalid-envelope"
    print("LIVE_PDF_SIDECAR_FAIL", json.dumps({"status": error.code, "stage": code}))
    raise
doc = body["data"]
receipt = doc["researchPdf"]
assert body["success"] is True
assert doc["metadata"]["sourceURL"] == URL
assert doc["metadata"]["url"].startswith("https://")
assert doc["metadata"]["contentType"] == "application/pdf"
assert doc["metadata"]["numPages"] == 1
assert doc["researchSourceVerified"] is False
assert receipt["source_verified"] is False
assert receipt["unresolved_pages"] == 0
assert len(receipt["pages"]) == 1
page = receipt["pages"][0]
assert page["page"] == 1 and page["source"] in ("native", "ocr")
for marker in ("Firecrawl synthetic PDF transport fixture", "Stock 421.5 million barrels",
               "Weekly change -1.2 million barrels", "September 8, 2026"):
    assert marker.lower() in " ".join(page["text"].split()).lower()
    assert marker.lower() in " ".join(doc["markdown"].split()).lower()
if page["source"] == "ocr":
    assert page["warning"] and isinstance(page["native_text"], str)
print("LIVE_PDF_SIDECAR_PASS", json.dumps({
    "source": "pinned synthetic PDF", "markers": 4, "pages": 1, "page_source": page["source"],
    "ocr_attempted": receipt["ocr_attempted"], "source_verified": False,
    "elapsed_ms": round((time.monotonic() - started) * 1000),
}))
