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

def call(path, data=None, mime="application/pdf", timeout=2, max_pages=None, page_range=None):
    headers={"Content-Type":mime} if data is not None else {}
    if max_pages is not None:
        headers["x-firecrawl-max-pages"]=str(max_pages)
    if page_range is not None:
        headers["x-firecrawl-page-range"]=page_range
    req=urllib.request.Request(BASE+path,data=data,headers=headers)
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
    assert result["total_pages"]==3 and result["requested_max_pages"] is None
    status,bounded=call("/extract",raw,timeout=75,max_pages=1)
    assert status==200 and bounded["total_pages"]==3 and bounded["requested_max_pages"]==1
    assert bounded["pages"]==[result["pages"][0]] and bounded["pages_needing_ocr"]==[]
    assert bounded["ocr_attempted"]==0 and bounded["unresolved_pages"]==0
    assert call("/extract",raw,max_pages=0)[0]==400
    assert call("/extract",raw,max_pages=201)[0]==400
    status,selected=call("/extract",raw,timeout=75,page_range="2-3")
    assert status==200 and selected["requested_page_range"]=={"start":2,"end":3}
    assert selected["pages"]==result["pages"][1:] and selected["pages_needing_ocr"]==[2,3]
    assert selected["total_pages"]==3 and selected["ocr_attempted"]==2
    status,outside=call("/extract",raw,timeout=10,page_range="4-5")
    assert status==200 and outside["pages"]==[] and outside["total_pages"]==3
    assert call("/extract",raw,page_range="3-2")[0]==400
    assert call("/extract",raw,max_pages=1,page_range="2-3")[0]==400
    print("OCR_HTTP_PAGE_RANGE_PASS: physical page numbers kept, conflicts rejected")
    print("OCR_HTTP_PAGE_BOUND_PASS: first native page only, two OCR pages omitted")
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

# Real ZIP bytes, signed number and explicit cell order in a minimal XLSX.
xlsx=ooxml({
    "[Content_Types].xml":'<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>',
    "_rels/.rels":'<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>',
    "xl/workbook.xml":'<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Stock" sheetId="1" r:id="rId1"/></sheets></workbook>',
    "xl/_rels/workbook.xml.rels":'<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>',
    "xl/worksheets/sheet1.xml":'<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:B2"/><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Stock</t></is></c><c r="B1" t="inlineStr"><is><t>Change</t></is></c></row><row r="2"><c r="A2"><v>421.5</v></c><c r="B2"><v>-1.2</v></c></row></sheetData></worksheet>',
})
status,result=call("/extract-office",xlsx,"application/octet-stream",timeout=20)
assert status==200 and result["parser"]=="upstream-anydoc" and result["source_verified"] is False
assert all(marker in result["markdown"] for marker in ("Stock","Change","421.5","-1.2"))
print("XLSX_HTTP_PROCESS_PASS")
# Unsafe and duplicate paths fail before the parser. No file extraction occurs.
unsafe=ooxml({"../escape.xml":"fixture","word/document.xml":"fixture"})
assert call("/extract-office",unsafe,"application/octet-stream",timeout=20)[0]==422
duplicate=io.BytesIO()
import warnings
with warnings.catch_warnings():
    warnings.simplefilter("ignore",UserWarning)
    with zipfile.ZipFile(duplicate,"w") as archive:
        archive.writestr("word/document.xml","one")
        archive.writestr("word/document.xml","two")
assert call("/extract-office",duplicate.getvalue(),"application/octet-stream",timeout=20)[0]==422
assert call("/health")[0]==200
print("OFFICE_ARCHIVE_GUARD_PASS")

