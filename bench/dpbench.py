"""Run our engine on DP-Bench (Upstage, MIT) and score it with the official
scorer, next to the commercial results shipped with the dataset.

    .venv/bin/python bench/dpbench.py [--skip-run]

Data lives in data/dp-bench (pdfs/, reference.json, evaluate.py, src/,
sample_results/). Predictions are written to data/dp-bench/preds/.
Coordinates are scaled from PDF points to the dataset's 200 DPI images.
"""
from __future__ import annotations

import argparse
import html
import json
import re
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DP = ROOT / "data" / "dp-bench"
PREDS = DP / "preds"
ENGINE = ROOT / "target" / "release" / "pagelens"
SCALE = 200 / 72
JOIN = "\n"  # line separator inside a paragraph; the scorer deletes "\n"


def quad(b: dict) -> list[dict]:
    x0, y0, x1, y1 = (b["x0"] * SCALE, b["y0"] * SCALE, b["x1"] * SCALE, b["y1"] * SCALE)
    return [{"x": x0, "y": y0}, {"x": x1, "y": y0}, {"x": x1, "y": y1}, {"x": x0, "y": y1}]


def inside(inner: dict, outer: dict, tol: float = 2.0) -> bool:
    return (inner["x0"] >= outer["x0"] - tol and inner["x1"] <= outer["x1"] + tol
            and inner["y0"] >= outer["y0"] - tol and inner["y1"] <= outer["y1"] + tol)


def paragraphs(segs: list[dict]) -> list[list[dict]]:
    """Group consecutive reading-order lines into blocks."""
    blocks: list[list[dict]] = []
    for s in segs:
        if blocks:
            p = blocks[-1][-1]
            b, pb = s["bbox"], p["bbox"]
            h = pb["y1"] - pb["y0"]
            gap = b["y0"] - pb["y1"]
            overlap = min(b["x1"], pb["x1"]) - max(b["x0"], pb["x0"])
            if -0.5 * h < gap < 0.6 * h and overlap > 0 and abs(s["size"] - p["size"]) < 1.0:
                blocks[-1].append(s)
                continue
        blocks.append([s])
    return blocks


def to_elements(result: dict) -> list[dict]:
    elements = []
    for page_no, page in enumerate(result["pages"], start=1):
        tables = page["tables"]
        # Tables first in page order of appearance, interleaved by position.
        items = []
        text_segs = [s for s in page["segments"] if not any(inside(s["bbox"], t["bbox"]) for t in tables)]
        for block in paragraphs(text_segs):
            bb = {"x0": min(s["bbox"]["x0"] for s in block), "y0": min(s["bbox"]["y0"] for s in block),
                  "x1": max(s["bbox"]["x1"] for s in block), "y1": max(s["bbox"]["y1"] for s in block)}
            items.append(("paragraph", bb, JOIN.join(s["text"] for s in block), ""))
        for t in tables:
            body = "".join("<tr>" + "".join(f"<td>{html.escape(c)}</td>" for c in row) + "</tr>"
                           for row in t["rows"])
            text = "\n".join(" ".join(c for c in row if c) for row in t["rows"])
            items.append(("table", t["bbox"], text, f"<table>{body}</table>"))
        # Keep reading order for text; place each table before the first
        # text block that starts below it.
        text_items = [i for i in items if i[0] == "paragraph"]
        for tbl in (i for i in items if i[0] == "table"):
            pos = next((k for k, i in enumerate(text_items) if i[1]["y0"] >= tbl[1]["y1"] - 1), len(text_items))
            text_items.insert(pos, tbl)
        for f in page.get("figures", []):
            text_items.append(("figure", f["bbox"], f["text"], ""))
        for cat, bb, text, h in text_items:
            elements.append({"category": cat, "page": page_no, "coordinates": quad(bb),
                             "content": {"text": text, "html": h, "markdown": ""}, "id": len(elements)})
    return elements


def run_engine() -> dict:
    pdfs = sorted((DP / "pdfs").glob("*.pdf"))

    def one(p: Path):
        t = time.perf_counter()
        res = subprocess.run([str(ENGINE), str(p)], capture_output=True, text=True)
        ms = (time.perf_counter() - t) * 1000
        if res.returncode != 0:
            return p.name, {"error": res.stderr.strip()}, ms
        return p.name, {"elements": to_elements(json.loads(res.stdout))}, ms

    t0 = time.perf_counter()
    with ThreadPoolExecutor(max_workers=1) as ex:  # sequential: per-page latency
        results = list(ex.map(one, pdfs))
    wall = time.perf_counter() - t0
    preds = {name: data for name, data, _ in results}
    lat = sorted(ms for *_, ms in results)
    print(f"engine: {len(pdfs)} pages in {wall:.1f}s, median {lat[len(lat)//2]:.0f} ms/page, "
          f"p95 {lat[int(len(lat)*0.95)]:.0f} ms (includes process start + PDFium load)")
    return preds


def score(pred_path: Path) -> tuple[float, float, float, float]:
    out = subprocess.run(
        [sys.executable, "evaluate.py", "--ref-path", "reference.json", "--pred-path", str(pred_path)],
        cwd=DP, capture_output=True, text=True,
    ).stdout
    def grab(label: str) -> float:
        m = re.search(re.escape(label) + r": ([0-9.]+)", out)
        return float(m.group(1)) if m else float("nan")
    return grab("NID Score"), grab("Table F1 Score"), grab("TEDS Score"), grab("TEDS-S Score")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--skip-run", action="store_true")
    ap.add_argument("--ours-only", action="store_true", help="skip re-scoring other systems")
    ap.add_argument("--join", default="\n", help="line separator inside paragraphs")
    args = ap.parse_args()
    global JOIN
    JOIN = args.join.encode().decode("unicode_escape")
    PREDS.mkdir(exist_ok=True)
    ours = PREDS / "ours.json"
    if not args.skip_run:
        ours.write_text(json.dumps(run_engine()))

    rows = [("pagelens (ours, no ML)", ours)]
    if not args.ours_only:
        rows += [(p.stem.replace("_260209", ""), p) for p in sorted((DP / "sample_results").glob("*_260209.json"))]
    docling = PREDS / "docling.json"
    if docling.exists() and not args.ours_only:
        rows.append(("docling", docling))
    print(f"\n{'system':<28} {'NID text+order':>15} {'table F1':>9} {'TEDS':>7} {'TEDS-S':>7}")
    for name, path in rows:
        nid, f1, teds, teds_s = score(path)
        print(f"{name:<28} {nid:>15.4f} {f1:>9.4f} {teds:>7.4f} {teds_s:>7.4f}")


if __name__ == "__main__":
    main()
