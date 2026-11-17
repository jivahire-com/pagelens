"""PyMuPDF predictions for DP-Bench, scored with the official scorer.

    .venv/bin/python bench/dpbench_pymupdf.py

Two variants:
  pymupdf_current  text blocks in file order, no tables (what the
                   automated_videos pipeline does today)
  pymupdf_best     blocks sorted top-left (sort=True) plus find_tables()
                   (the best PyMuPDF can do out of the box)
"""
from __future__ import annotations

import html
import json
import sys
from pathlib import Path

import fitz

ROOT = Path(__file__).resolve().parent.parent
DP = ROOT / "data" / "dp-bench"
SCALE = 200 / 72


def quad(r) -> list[dict]:
    x0, y0, x1, y1 = (v * SCALE for v in r)
    return [{"x": x0, "y": y0}, {"x": x1, "y": y0}, {"x": x1, "y": y1}, {"x": x0, "y": y1}]


def elements(pdf: Path, best: bool) -> list[dict]:
    out = []
    with fitz.open(pdf) as doc:
        for pno, page in enumerate(doc, start=1):
            table_boxes = []
            if best:
                for t in page.find_tables().tables:
                    rows = t.extract()
                    body = "".join("<tr>" + "".join(f"<td>{html.escape(c or '')}</td>" for c in r) + "</tr>"
                                   for r in rows)
                    text = "\n".join(" ".join(c or "" for c in r) for r in rows)
                    out.append({"category": "table", "page": pno, "coordinates": quad(t.bbox),
                                "content": {"text": text, "html": f"<table>{body}</table>", "markdown": ""}})
                    table_boxes.append(fitz.Rect(t.bbox))
            for b in page.get_text("blocks", sort=best):
                if b[6] != 0 or not b[4].strip():
                    continue
                r = fitz.Rect(b[:4])
                if any(tb.contains(r) for tb in table_boxes):
                    continue
                out.append({"category": "paragraph", "page": pno, "coordinates": quad(b[:4]),
                            "content": {"text": b[4].strip(), "html": "", "markdown": ""}})
    for i, e in enumerate(out):
        e["id"] = i
    return out


def main() -> None:
    (DP / "preds").mkdir(exist_ok=True)
    for name, best in (("pymupdf_current", False), ("pymupdf_best", True)):
        preds = {p.name: {"elements": elements(p, best)} for p in sorted((DP / "pdfs").glob("*.pdf"))}
        (DP / "preds" / f"{name}.json").write_text(json.dumps(preds))
        print(f"wrote preds/{name}.json", file=sys.stderr)


if __name__ == "__main__":
    main()
