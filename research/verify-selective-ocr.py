"""Exercise the real Rust OCR worker with synthetic PDFs in network-none CI."""
import json
import subprocess
import tempfile
from pathlib import Path

from ocr_benchmark import fixture, score

def run(binary, pdf, *args):
    p=subprocess.run([binary,*args], input=pdf, capture_output=True,check=True,timeout=60)
    return json.loads(p.stdout)

def main():
    with tempfile.TemporaryDirectory(prefix="selective-ocr-") as td:
        pdf=Path(td)/"mixed.pdf"
        fixture(pdf)
        data=pdf.read_bytes()
        reference=run("/usr/local/bin/pdf-page-probe",data)
        assert reference["pages_needing_ocr"]==[2,3]
        native=reference["pages"][0]["markdown"]
        assert "Native text control" in native and "2026-09-08" in native

        output=run("/usr/local/bin/pdf-ocr-worker",data,"--single-block-fixture")
        assert output["pages_needing_ocr"]==[2,3]
        assert output["ocr_attempted"]==2 and output["unresolved_pages"]==0
        assert output["source_verified"] is False
        assert output["layout"]=="single_block"
        assert output["pages"][0]=={"page":1,"text":native,"source":"native","warning":None}
        for p in output["pages"][1:]:
            metrics=score(p["text"])
            print(json.dumps({"page":p["page"],"source":p["source"],"warning":p["warning"],
                "date_ok":metrics["date_ok"],"rows_exact":metrics["rows_exact"]}))
            assert p["source"]=="ocr" and p["warning"]
            assert metrics["date_ok"] and metrics["rows_exact"]==metrics["rows_expected"]

        auto=run("/usr/local/bin/pdf-ocr-worker",data)
        assert auto["layout"]=="auto" and auto["source_verified"] is False
        assert auto["pages"][0]==output["pages"][0]
        assert auto["pages_needing_ocr"]==[2,3] and auto["ocr_attempted"]==2
        assert all(p["warning"] for p in auto["pages"][1:])
        bad=subprocess.run(["/usr/local/bin/pdf-ocr-worker"],input=b"not a PDF",
            capture_output=True,timeout=5)
        assert bad.returncode!=0 and not bad.stdout
        print("SELECTIVE OCR FIXTURE PASS: Rust worker; 1 native page untouched; 2 OCR pages; 8/8 rows; unverified OCR flagged")

if __name__=="__main__":
    main()
