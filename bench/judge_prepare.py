"""Build the judge sample set: one-page PDFs, page images and parser outputs.

    .venv/bin/python bench/judge_prepare.py

Sample (30 pages, no real resumes):
  20 DP-Bench pages: 8 with tables, 6 of our weakest pages by NID, 6 others
     with multi-column text (fixed seed, so the set is reproducible).
  10 synthetic resume first pages spread across the templates.

Writes bench/judge/samples/<id>/{page.pdf,page.png,ours.txt,pymupdf.txt}
and then runs Docling (its own venv) to add docling.txt.
"""
from __future__ import annotations

import json
import random
import subprocess
from pathlib import Path

import fitz

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "bench" / "judge" / "samples"
DP = ROOT / "data" / "dp-bench"
ENGINE = ROOT / "target" / "release" / "pagelens"
# Long side of the page image sent to the judge (vision models downscale
# beyond ~1568 px anyway).
IMAGE_LONG_SIDE = 1568


def pick_dpbench() -> list[str]:
    rng = random.Random(7)
    ref = json.loads((DP / "reference.json").read_text())
    ev = json.loads((DP / "preds" / "ours.eval.json").read_text())["per_image_results"]
    with_tables = sorted(k for k in ref if any(e["category"] == "Table" for e in ref[k]["elements"]))
    weakest = [k for _, k in sorted((v["layout"]["nid_score"], k) for k, v in ev.items())]
    picked: list[str] = []
    for k in rng.sample(with_tables, 8):
        picked.append(k)
    for k in weakest:
        if len(picked) >= 14:
            break
        if k not in picked and k not in ("01030000000141.pdf",):  # skip the poster OCR page
            picked.append(k)
    rest = [k for k in sorted(ref) if k not in picked]
    picked += rng.sample(rest, 20 - len(picked))
    return picked


def pick_resumes() -> list[tuple[str, Path]]:
    private = ROOT / "data" / "private_resumes"
    ids = json.loads((private / "id_map.json").read_text())
    chosen = ["R01", "R06", "R13", "R23", "R33", "R36", "R40", "R45", "R46", "R48"]
    return [(rid, private / "extracted" / ids[rid]) for rid in chosen]


def one_page_pdf(src: Path, dst: Path) -> None:
    with fitz.open(src) as doc:
        out = fitz.open()
        out.insert_pdf(doc, from_page=0, to_page=0)
        out.save(dst)


def render(pdf: Path, png: Path) -> None:
    with fitz.open(pdf) as doc:
        page = doc[0]
        zoom = IMAGE_LONG_SIDE / max(page.rect.width, page.rect.height)
        page.get_pixmap(matrix=fitz.Matrix(zoom, zoom)).save(png)


def ours_text(pdf: Path) -> str:
    out = subprocess.run([str(ENGINE), str(pdf)], capture_output=True, text=True, check=True)
    return json.loads(out.stdout)["text"]


def pymupdf_text(pdf: Path) -> str:
    with fitz.open(pdf) as doc:
        return doc[0].get_text("text")


DOCLING_SNIPPET = """
import sys
from pathlib import Path
from docling.backend.pypdfium2_backend import PyPdfiumDocumentBackend
from docling.datamodel.accelerator_options import AcceleratorDevice, AcceleratorOptions
from docling.datamodel.base_models import InputFormat
from docling.datamodel.pipeline_options import PdfPipelineOptions
from docling.document_converter import DocumentConverter, PdfFormatOption
o = PdfPipelineOptions(accelerator_options=AcceleratorOptions(device=AcceleratorDevice.CPU))
c = DocumentConverter(format_options={InputFormat.PDF: PdfFormatOption(pipeline_options=o, backend=PyPdfiumDocumentBackend)})
for d in sys.argv[1:]:
    d = Path(d)
    md = c.convert(d / "page.pdf").document.export_to_markdown()
    (d / "docling.txt").write_text(md)
    print(d.name, flush=True)
"""


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    samples: list[tuple[str, Path]] = [(f"dp_{k[:-4][-3:]}", DP / "pdfs" / k) for k in pick_dpbench()]
    samples += [(f"cv_{rid}", path) for rid, path in pick_resumes()]
    for sid, src in samples:
        d = OUT / sid
        d.mkdir(exist_ok=True)
        one_page_pdf(src, d / "page.pdf")
        render(d / "page.pdf", d / "page.png")
        (d / "ours.txt").write_text(ours_text(d / "page.pdf"))
        (d / "pymupdf.txt").write_text(pymupdf_text(d / "page.pdf"))
    (OUT.parent / "manifest.json").write_text(json.dumps({sid: str(src.relative_to(ROOT)) for sid, src in samples}, indent=1))
    todo = [str(OUT / sid) for sid, _ in samples if not (OUT / sid / "docling.txt").exists()]
    if todo:
        subprocess.run([str(ROOT / ".venv-docling" / "bin" / "python"), "-c", DOCLING_SNIPPET, *todo], check=True,
                       stderr=subprocess.DEVNULL)
    print(f"prepared {len(samples)} samples in {OUT}")


if __name__ == "__main__":
    main()
