"""Exercise PID-1 OCR server and its actual worker in a network-none container."""
import json
import io
import zipfile
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

# Minimal valid OOXML, generated entirely inside the network-none container.
def ooxml(parts):
    out=io.BytesIO()
    with zipfile.ZipFile(out,"w",zipfile.ZIP_DEFLATED) as archive:
        for name,value in parts.items():
            archive.writestr(name,value)
    return out.getvalue()

docx=ooxml({
    "[Content_Types].xml":'<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>',
    "_rels/.rels":'<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>',
    "word/document.xml":'<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Office fixture stock 421.5 change -1.2</w:t></w:r></w:p></w:body></w:document>',
})
assert call("/extract-office",b"not document","application/octet-stream")[0]==400
assert call("/extract-office",b"%PDF-fixture","application/pdf")[0]==415
status,result=call("/extract-office",docx,"application/octet-stream",timeout=20)
assert status==200 and result["parser"]=="upstream-anydoc" and result["source_verified"] is False
assert all(marker in result["markdown"] for marker in ("Office fixture","421.5","-1.2"))
print("OFFICE_HTTP_PROCESS_PASS")
assert call("/health")[0]==200
