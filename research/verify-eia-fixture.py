"""Strict offline EIA snapshot regression. All dates and values must stay aligned."""
import json
import re
import subprocess
from pathlib import Path
from html import unescape

ROOT = Path(__file__).resolve().parent
source = (ROOT / "fixtures/eia-stock-table.html").read_text(encoding="utf-8")
expected = json.loads((ROOT / "fixtures/eia-stock-table.expected.json").read_text(encoding="utf-8"))
assert len(expected["dates"]) == 6 and len(expected["rows"]) == 39
assert all(len(row) == 7 for row in expected["rows"])
# The source labels were captured as HTML text; decode HTML entities once.
expected["rows"] = [[unescape(row[0]), *row[1:]] for row in expected["rows"]]

def norm(s):
    s = re.sub(r'!?\[([^\]]*)\]\([^)]*\)', r'\1', s)
    s = unescape(s).replace('**', '').replace('\\|', '|').replace('\\_', '_').replace('\u2212', '-').strip()
    # Markdown escapes a leading > to avoid starting a blockquote. Preserve
    # the comparison operator itself; do not normalize numeric data away.
    if s.startswith(r'\>'):
        s = s[1:]
    return ' '.join(s.split())

def blocks(md):
    block = []
    for line in md.splitlines():
        line = line.strip()
        if line.startswith('|') and line.endswith('|'):
            cells = [norm(x) for x in re.split(r'(?<!\\)\|', line[1:-1])]
            if not all(re.fullmatch(r':?-+:?', x) for x in cells):
                block.append(cells)
        elif block:
            yield block
            block = []
    if block:
        yield block

def verify(md):
    matches = []
    for block in blocks(md):
        for i, header in enumerate(block):
            for pos in range(1, max(1, len(header)-5)):
                if header[pos:pos+6] != expected["dates"]:
                    continue
                # Include blank-value rows, not just numeric hits. No
                # cross-table or cross-row matching; order and multiplicity matter.
                body = block[i+1:]
                if any(len(row) != len(header) for row in body):
                    raise AssertionError("ragged EIA data row")
                rows = [[' '.join(x for x in row[:pos] if x), *row[pos:pos+6]] for row in body]
                matches.append(rows)
    if matches != [expected["rows"]]:
        best = max(matches, key=lambda rows: sum(a == b for a,b in zip(rows, expected["rows"])), default=[])
        diffs = [{"index":i, "expected":e, "actual":a} for i,(e,a) in enumerate(zip(expected["rows"],best)) if e!=a][:3]
        raise AssertionError(f"table grid mismatch: matching headers={len(matches)}, rows={len(best)}, first mismatches={diffs}")
    return {"rows":39, "cells":234, "dates":6}

# The pinned candidate is run with no network or filesystem mount.
for main in (True, False):
    payload = json.dumps({"url":expected["source"],"content":source,"onlyMainContent":main}).encode()
    proc = subprocess.run(["docker","run","--rm","-i","--pull=never","--network","none","--read-only",
        "--user","65532:65532","--cap-drop","ALL","--security-opt","no-new-privileges",
        "--memory","1g","--pids-limit","64","--cpus","2","firecrawl-rust-table-ci:review"],
        input=payload,capture_output=True,check=True,timeout=45)
    md = json.loads(proc.stdout)["markdown"]
    assert isinstance(md, str)
    print(json.dumps({"main":main,"verdict":"PASS",**verify(md)}))
