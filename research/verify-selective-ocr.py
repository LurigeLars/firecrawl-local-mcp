"""Offline selective OCR research test, not a production PDF parser.

Synthetic table pages use a known single-block layout. Successful Tesseract
execution does NOT certify arbitrary scanned financial values or tables.
"""
import json
import os
import subprocess
import tempfile
from pathlib import Path

from PIL import Image
from ocr_benchmark import fixture, score

def probe(pdf):
    p = subprocess.run(["/usr/local/bin/pdf-page-probe"], input=pdf.read_bytes(),
                       capture_output=True, check=True, timeout=30)
    return json.loads(p.stdout)

def merge_pages(result, render_ocr):
    pages = result["pages"]
    assert [p["page"] for p in pages] == list(range(1, len(pages) + 1))
    flagged = [p["page"] for p in pages if p["needs_ocr"]]
    assert flagged == result["pages_needing_ocr"]
    assert len(flagged) <= 4, "research OCR page budget exceeded"
    output = []
    for page in pages:
        if not page["needs_ocr"]:
            output.append({"page": page["page"], "text": page["markdown"],
                           "source": "native", "warning": None})
            continue
        try:
            text = render_ocr(page["page"])
            if not text.strip():
                raise ValueError("empty OCR result")
            # Do not mark OCR output as verified source data.
            output.append({"page": page["page"], "text": text,
                           "source": "ocr", "warning": "OCR text not independently verified"})
        except (OSError, ValueError, subprocess.SubprocessError):
            output.append({"page": page["page"], "text": page["markdown"],
                           "source": "unresolved", "warning": "OCR unavailable or incomplete"})
    return output

def main():
    with tempfile.TemporaryDirectory(prefix="selective-ocr-") as td:
        root = Path(td)
        pdf = root / "mixed.pdf"
        fixture(pdf)
        result = probe(pdf)
        print(json.dumps({"pages_needing_ocr": result["pages_needing_ocr"],
                          "classification": [{"page": p["page"], "needs_ocr": p["needs_ocr"],
                                              "reason": p["ocr_reason"]} for p in result["pages"]]}))
        assert result["pages_needing_ocr"] == [2, 3], "incorrect selective page classification"
        native = result["pages"][0]["markdown"]
        assert "Native text control" in native and "2026-09-08" in native
        calls = []

        def render(page):
            calls.append(page)
            prefix = root / f"page-{page}"
            env = {**os.environ, "OMP_THREAD_LIMIT": "1"}
            # Bound both axes in the renderer. 2200px long edge is roughly
            # 200dpi for these Letter-sized fixtures, not an exact DPI setting.
            subprocess.run(["pdftoppm", "-f", str(page), "-l", str(page),
                            "-scale-to", "2200", "-singlefile", "-gray", "-png",
                            str(pdf), str(prefix)], capture_output=True, check=True,
                           timeout=15, env=env)
            img = prefix.with_suffix(".png")
            with Image.open(img) as im:
                assert max(im.size) <= 2200
            # PSM6 is chosen ONLY for the known fixture layout.
            p = subprocess.run(["tesseract", str(img), "stdout", "-l", "eng", "--psm", "6"],
                               capture_output=True, check=True, timeout=15, env=env)
            return p.stdout.decode("utf-8", "strict")

        output = merge_pages(result, render)
        assert calls == [2, 3]
        assert output[0] == {"page": 1, "text": native, "source": "native", "warning": None}
        for p in output[1:]:
            metrics = score(p["text"])
            print(json.dumps({"page": p["page"], "source": p["source"], "warning": p["warning"],
                              "date_ok": metrics["date_ok"], "rows_exact": metrics["rows_exact"]}))
            assert p["source"] == "ocr" and p["warning"]
            assert metrics["date_ok"] and metrics["rows_exact"] == metrics["rows_expected"]

        def failed(page):
            raise subprocess.TimeoutExpired("synthetic-ocr", 1)

        failed_result = merge_pages(result, failed)
        assert failed_result[0] == output[0]
        assert all(p["source"] == "unresolved" and p["warning"] for p in failed_result[1:])
        print("SELECTIVE OCR FIXTURE PASS: 1 native page untouched; 2 OCR pages; 8/8 rows; timeout warnings")

if __name__ == "__main__":
    main()
