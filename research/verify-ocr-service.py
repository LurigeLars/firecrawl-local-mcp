"""Exercise PID-1 OCR server and its actual worker in a network-none container."""
import json
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path
from ocr_benchmark import fixture

BASE="http://127.0.0.1:3000"
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))

def call(path, data=None, mime="application/pdf", timeout=2):
    req=urllib.request.Request(BASE+path,data=data,
        headers={"Content-Type":mime} if data is not None else {})
    try:
        with opener.open(req,timeout=timeout) as res:
            return res.status,json.loads(res.read(2*1024*1024+512))
    except urllib.error.HTTPError as err:
        return err.code,json.loads(err.read(4096))

for _ in range(50):
    try:
        if call("/health")[0]==200:
            break
    except (OSError,urllib.error.URLError):
        pass
    time.sleep(.1)
else:
    raise AssertionError("OCR server not ready")
assert call("/extract",b"not PDF")[0]==400
assert call("/extract",b"%PDF-fixture","text/plain")[0]==415

with tempfile.TemporaryDirectory(prefix="fc-ocr-http-") as tmp:
    pdf=Path(tmp)/"mixed.pdf"
    fixture(pdf)
    raw=pdf.read_bytes()
    ref=json.loads(subprocess.run(["/usr/local/bin/pdf-page-probe"],
        input=raw,capture_output=True,check=True,timeout=10).stdout)
    status,result=call("/extract",raw,timeout=75)
    assert status==200
    assert result["source_verified"] is False and result["layout"]=="auto"
    assert result["pages_needing_ocr"]==[2,3] and result["ocr_attempted"]==2
    assert result["pages"][0]["source"]=="native"
    assert result["pages"][0]["text"]==ref["pages"][0]["markdown"]
    assert all(p["warning"] and p["source"] in ("ocr","unresolved") for p in result["pages"][1:])
    assert result["unresolved_pages"]==sum(p["source"]=="unresolved" for p in result["pages"])
    print("OCR_HTTP_PROCESS_PASS: native text preserved, OCR uncertainty and per-page accounting preserved")
    print(json.dumps({"ocr_attempted":result["ocr_attempted"],"unresolved":result["unresolved_pages"],
        "page_sources":[p["source"] for p in result["pages"]]}))
assert call("/health")[0]==200
