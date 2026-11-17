"""Private visual report for the resume set. Opens locally, nothing uploaded.

    .venv/bin/python bench/resume_report.py
    open data/private_resumes/report/index.html

Per resume: each page image with our reading order (numbered boxes) and
detected tables (red), followed by our text, Docling's text and PyMuPDF's
text side by side. Written only inside data/private_resumes/ (git-ignored).
"""
from __future__ import annotations

import base64
import html
import json
import subprocess
from pathlib import Path

import fitz

ROOT = Path(__file__).resolve().parent.parent
PRIVATE = ROOT / "data" / "private_resumes"
ENGINE = ROOT / "target" / "release" / "pagelens"
ZOOM = 1.3

CSS = """
body{font:14px system-ui,sans-serif;margin:16px;background:#f6f7f9;color:#1d2330}
h2{margin:32px 0 8px} .row{display:flex;gap:12px;align-items:flex-start;flex-wrap:wrap}
.page{position:relative;display:inline-block;box-shadow:0 1px 4px #0003;background:#fff}
.page svg{position:absolute;left:0;top:0}
pre{white-space:pre-wrap;background:#fff;border:1px solid #d8dce3;padding:8px;width:31%;min-width:280px;
    max-height:900px;overflow:auto;font-size:12px;margin:0}
.lbl{font-weight:600;margin:6px 0} nav a{margin-right:8px}
"""


def page_svg(page_result: dict, w: float, h: float) -> str:
    parts = [f'<svg width="{w * ZOOM}" height="{h * ZOOM}" viewBox="0 0 {w} {h}">']
    for t in page_result["tables"]:
        b = t["bbox"]
        parts.append(f'<rect x="{b["x0"]}" y="{b["y0"]}" width="{b["x1"]-b["x0"]}" height="{b["y1"]-b["y0"]}" '
                     'fill="rgba(220,40,40,.08)" stroke="#d22" stroke-width="1.2"/>')
    for i, s in enumerate(page_result["segments"], start=1):
        b = s["bbox"]
        parts.append(f'<rect x="{b["x0"]}" y="{b["y0"]}" width="{b["x1"]-b["x0"]}" height="{b["y1"]-b["y0"]}" '
                     'fill="none" stroke="#2563eb" stroke-width=".5"/>')
        parts.append(f'<text x="{b["x0"]-1}" y="{b["y0"]+4}" font-size="5" fill="#2563eb" text-anchor="end">{i}</text>')
    parts.append("</svg>")
    return "".join(parts)


def main() -> None:
    ids = json.loads((PRIVATE / "id_map.json").read_text())
    out = PRIVATE / "report"
    out.mkdir(exist_ok=True)
    body = ['<nav>' + "".join(f'<a href="#{rid}">{rid}</a>' for rid in sorted(ids)) + "</nav>"]
    for rid, rel in sorted(ids.items()):
        pdf = PRIVATE / "extracted" / rel
        ours = json.loads(subprocess.run([str(ENGINE), str(pdf)], capture_output=True, text=True).stdout)
        dl_path = PRIVATE / "cache" / "docling" / f"{rid}.json"
        docling = json.loads(dl_path.read_text())["text"] if dl_path.exists() else "(not run yet)"
        with fitz.open(pdf) as doc:
            mu = "\n".join(p.get_text("text") for p in doc)
            body.append(f'<h2 id="{rid}">{rid}</h2><div class="row">')
            for pno, page in enumerate(doc):
                png = page.get_pixmap(matrix=fitz.Matrix(ZOOM, ZOOM)).tobytes("png")
                b64 = base64.b64encode(png).decode()
                svg = page_svg(ours["pages"][pno], page.rect.width, page.rect.height)
                body.append(f'<div class="page"><img src="data:image/png;base64,{b64}">{svg}</div>')
            body.append("</div><div class=\"row\">")
        for label, text in (("pagelens (ours)", ours["text"]), ("Docling", docling), ("PyMuPDF (current)", mu)):
            body.append(f'<pre><div class="lbl">{label}</div>{html.escape(text)}</pre>')
        body.append("</div>")
    page = f"<!doctype html><meta charset=utf-8><title>Resume parser review</title><style>{CSS}</style>{''.join(body)}"
    (out / "index.html").write_text(page)
    print(f"wrote {out / 'index.html'}")


if __name__ == "__main__":
    main()
