"""Run Docling over our synthetic corpus and DP-Bench, caching its output.

    .venv-docling/bin/python bench/run_docling.py [corpus|dpbench|all] [--pypdfium]

Docling runs with its default settings: layout and TableFormer models, OCR
where needed, on CPU. `--pypdfium` swaps in Docling's PyPdfium backend for
machines without a docling-parse build (e.g. macOS 12 on Intel). Docling's
memory grows over a long run; on a small machine, run slices of DP-Bench in
fresh processes and merge the predictions.

Outputs:
  bench/cache/docling/<name>.json           {"text", "tables", "ms"}  (corpus)
  data/dp-bench/preds/docling.json          official DP-Bench format
"""
from __future__ import annotations

import json
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CORPUS = ROOT / "corpus"
CACHE = ROOT / "bench" / "cache" / "docling"
DP = ROOT / "data" / "dp-bench"
SCALE = 200 / 72

LABELS = {
    "section_header": "heading1", "title": "heading1", "page_header": "header",
    "page_footer": "footer", "caption": "caption", "footnote": "footnote",
    "list_item": "list", "formula": "equation", "table": "table",
    "picture": "figure", "chart": "chart",
}


def converter():
    from docling.backend.pypdfium2_backend import PyPdfiumDocumentBackend
    from docling.datamodel.accelerator_options import AcceleratorDevice, AcceleratorOptions
    from docling.datamodel.base_models import InputFormat
    from docling.datamodel.pipeline_options import PdfPipelineOptions
    from docling.document_converter import DocumentConverter, PdfFormatOption

    opts = PdfPipelineOptions(accelerator_options=AcceleratorOptions(device=AcceleratorDevice.CPU))
    if "--pypdfium" not in sys.argv:
        return DocumentConverter(format_options={InputFormat.PDF: PdfFormatOption(pipeline_options=opts)})
    return DocumentConverter(format_options={
        InputFormat.PDF: PdfFormatOption(pipeline_options=opts, backend=PyPdfiumDocumentBackend)})


def table_rows(table, doc) -> list[list[str]]:
    df = table.export_to_dataframe(doc=doc)
    header = [str(c) for c in df.columns]
    rows = [[("" if v is None else str(v)) for v in r] for r in df.values.tolist()]
    if all(h.isdigit() for h in header):  # Docling numbers columns when there is no header
        return rows
    return [header] + rows


def timed_convert(conv, pdf: Path):
    t = time.perf_counter()
    doc = conv.convert(pdf).document
    return doc, (time.perf_counter() - t) * 1000


def run_corpus(conv) -> None:
    CACHE.mkdir(parents=True, exist_ok=True)
    for pdf in sorted(CORPUS.glob("*.pdf")):
        doc, ms = timed_convert(conv, pdf)
        text = doc.export_to_text() if hasattr(doc, "export_to_text") else doc.export_to_markdown()
        tables = [table_rows(t, doc) for t in doc.tables]
        (CACHE / f"{pdf.stem}.json").write_text(json.dumps({"text": text, "tables": tables, "ms": ms}))
        print(f"{pdf.stem:<40} {ms:8.0f} ms  tables={len(tables)}", flush=True)


def to_quad(prov, page_h: float) -> list[dict]:
    b = prov.bbox.to_top_left_origin(page_height=page_h)
    x0, y0, x1, y1 = b.l * SCALE, b.t * SCALE, b.r * SCALE, b.b * SCALE
    return [{"x": x0, "y": y0}, {"x": x1, "y": y0}, {"x": x1, "y": y1}, {"x": x0, "y": y1}]


def run_dpbench(conv) -> None:
    from docling_core.types.doc import TableItem, TextItem

    preds, lat = {}, []
    for pdf in sorted((DP / "pdfs").glob("*.pdf")):
        try:
            doc, ms = timed_convert(conv, pdf)
        except Exception as e:  # recorded as an error, as the scorer expects
            preds[pdf.name] = {"error": str(e)}
            continue
        lat.append(ms)
        elements = []
        for item, _level in doc.iterate_items():
            if not getattr(item, "prov", None):
                continue
            prov = item.prov[0]
            page_h = doc.pages[prov.page_no].size.height
            label = str(getattr(item.label, "value", item.label))
            cat = LABELS.get(label, "paragraph")
            if isinstance(item, TableItem):
                text = "\n".join(" ".join(r) for r in table_rows(item, doc))
                html = item.export_to_html(doc=doc)
            elif isinstance(item, TextItem):
                text, html = item.text, ""
            else:
                text, html = "", ""
            elements.append({"category": cat, "page": prov.page_no, "id": len(elements),
                             "coordinates": to_quad(prov, page_h),
                             "content": {"text": text, "html": html, "markdown": ""}})
        preds[pdf.name] = {"elements": elements}
        print(f"{pdf.name:<24} {ms:8.0f} ms  elements={len(elements)}", flush=True)
    (DP / "preds").mkdir(exist_ok=True)
    (DP / "preds" / "docling.json").write_text(json.dumps(preds))
    lat.sort()
    print(f"docling: median {lat[len(lat)//2]:.0f} ms/page, p95 {lat[int(len(lat)*0.95)]:.0f} ms (models preloaded)")


RESUME_SETS = {  # name -> (id map, source folder, cache folder)
    "synthetic": ("id_map.json", "extracted", "docling"),
    "real": ("id_map_real.json", "real", "docling_real"),
}


def run_resumes(conv, which: str = "synthetic") -> None:
    """Private resumes: cache text under the private folder, anonymous IDs."""
    private = ROOT / "data" / "private_resumes"
    id_file, src, cache = RESUME_SETS[which]
    ids = json.loads((private / id_file).read_text())
    out = private / "cache" / cache
    out.mkdir(parents=True, exist_ok=True)
    for rid, rel in sorted(ids.items()):
        try:
            doc, ms = timed_convert(conv, private / src / rel)
            text = doc.export_to_text() if hasattr(doc, "export_to_text") else doc.export_to_markdown()
        except Exception as e:
            text, ms = "", 0.0
            print(f"{rid} failed: {type(e).__name__}", flush=True)
        (out / f"{rid}.json").write_text(json.dumps({"text": text, "ms": ms}))
        print(f"{rid} {ms:8.0f} ms", flush=True)


def main() -> None:
    which = sys.argv[1] if len(sys.argv) > 1 else "all"
    t0 = time.perf_counter()
    conv = converter()
    conv.convert(sorted(CORPUS.glob("*.pdf"))[0])  # warm-up: load models once
    print(f"models loaded + warm-up in {time.perf_counter() - t0:.1f}s", flush=True)
    if which in ("corpus", "all"):
        run_corpus(conv)
    if which in ("dpbench", "all"):
        run_dpbench(conv)
    if which == "resumes":
        run_resumes(conv, sys.argv[2] if len(sys.argv) > 2 else "synthetic")


if __name__ == "__main__":
    main()
