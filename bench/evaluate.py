"""Score PDF extractors against the ground truth in corpus/.

Reading-order documents:
  line_integrity  share of ground-truth lines found intact in the output
  order           share of found line pairs that appear in the correct order
  similarity      normalised edit similarity of the whole text
Table documents:
  row_match       share of ground-truth rows reproduced exactly as a row
  shape           1 if the best table has the right rows x columns

Usage: python bench/evaluate.py [--only name1,name2] [--engine path/to/binary]
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import time
from pathlib import Path
from statistics import mean
from typing import Callable

from rapidfuzz.distance import Levenshtein

ROOT = Path(__file__).resolve().parent.parent
CORPUS = ROOT / "corpus"


def norm(s: str) -> str:
    return re.sub(r"\s+", " ", s or "").strip().lower()


# ------------------------------------------------------------ text extractors

def pymupdf_text(path: Path) -> str:
    """What automated_videos uses in production today."""
    import fitz
    with fitz.open(path) as doc:
        return "\n".join(p.get_text("text") for p in doc)


def pymupdf_sort(path: Path) -> str:
    import fitz
    with fitz.open(path) as doc:
        return "\n".join(p.get_text("text", sort=True) for p in doc)


def pdfplumber_text(path: Path) -> str:
    import pdfplumber
    with pdfplumber.open(path) as pdf:
        return "\n".join((p.extract_text() or "") for p in pdf.pages)


def pdfium_text(path: Path) -> str:
    import pypdfium2 as pdfium
    pdf = pdfium.PdfDocument(str(path))
    try:
        return "\n".join(pdf[i].get_textpage().get_text_range() for i in range(len(pdf)))
    finally:
        pdf.close()


# ----------------------------------------------------------- table extractors

def pymupdf_tables(strategy: str) -> Callable[[Path], list]:
    def run(path: Path) -> list:
        import fitz
        out = []
        with fitz.open(path) as doc:
            for p in doc:
                for t in p.find_tables(strategy=strategy).tables:
                    out.append(t.extract())
        return out
    return run


def pdfplumber_tables(strategy: str) -> Callable[[Path], list]:
    def run(path: Path) -> list:
        import pdfplumber
        settings = {"vertical_strategy": strategy, "horizontal_strategy": strategy}
        out = []
        with pdfplumber.open(path) as pdf:
            for p in pdf.pages:
                out.extend(p.extract_tables(settings))
        return out
    return run


def engine_runner(binary: Path) -> tuple[Callable[[Path], str], Callable[[Path], list]]:
    """Our Rust engine: `<binary> <pdf>` prints JSON {text, tables}."""
    def call(path: Path) -> dict:
        res = subprocess.run([str(binary), str(path)], capture_output=True, text=True, check=True)
        return json.loads(res.stdout)
    return (lambda p: call(p)["text"]), (lambda p: call(p)["tables"])


def docling_cached() -> tuple[Callable[[Path], str], Callable[[Path], list], Callable[[Path], float]]:
    """Docling output cached by bench/run_docling.py (runs in its own venv)."""
    cache = ROOT / "bench" / "cache" / "docling"
    load = lambda p: json.loads((cache / f"{p.stem}.json").read_text())
    return (lambda p: load(p)["text"]), (lambda p: load(p)["tables"]), (lambda p: load(p)["ms"])


# ------------------------------------------------------------------- metrics

def score_reading(truth: list[str], output: str) -> dict:
    out = norm(output)
    positions = []
    found = 0
    for line in truth:
        idx = out.find(norm(line))
        if idx >= 0:
            found += 1
            positions.append(idx)
    pairs = good = 0
    for i in range(len(positions)):
        for j in range(i + 1, len(positions)):
            pairs += 1
            good += positions[i] < positions[j]
    return {
        "line_integrity": found / len(truth),
        "order": good / pairs if pairs else 0.0,
        "similarity": Levenshtein.normalized_similarity(norm(" ".join(truth)), out),
    }


def score_table(truth: list[list[str]], tables: list) -> dict:
    want = [tuple(norm(c) for c in row) for row in truth]
    best = {"row_match": 0.0, "shape": 0.0}
    for t in tables:
        rows = [tuple(norm(c or "") for c in row) for row in t]
        have = set(rows)
        rm = sum(r in have for r in want) / len(want)
        shape = float(len(rows) == len(want) and all(len(r) == len(want[0]) for r in rows))
        if (rm, shape) > (best["row_match"], best["shape"]):
            best = {"row_match": rm, "shape": shape}
    return best


# ---------------------------------------------------------------------- main

def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--only", help="comma-separated extractor names")
    ap.add_argument("--engine", type=Path, help="path to our engine binary")
    args = ap.parse_args()

    text_ex: dict[str, Callable] = {
        "pymupdf_text (current)": pymupdf_text,
        "pymupdf_sort": pymupdf_sort,
        "pdfplumber": pdfplumber_text,
        "pdfium": pdfium_text,
    }
    table_ex: dict[str, Callable] = {
        "pymupdf_tables_lines": pymupdf_tables("lines"),
        "pymupdf_tables_text": pymupdf_tables("text"),
        "pdfplumber_tables_lines": pdfplumber_tables("lines"),
        "pdfplumber_tables_text": pdfplumber_tables("text"),
    }
    if args.engine:
        t, tb = engine_runner(args.engine)
        text_ex["ours"], table_ex["ours"] = t, tb
    reported_ms: dict[str, Callable[[Path], float]] = {}
    if (ROOT / "bench" / "cache" / "docling").exists():
        t, tb, ms = docling_cached()
        text_ex["docling"], table_ex["docling"] = t, tb
        reported_ms["docling"] = ms
    if args.only:
        keep = set(args.only.split(","))
        text_ex = {k: v for k, v in text_ex.items() if k in keep}
        table_ex = {k: v for k, v in table_ex.items() if k in keep}

    docs = [json.loads(p.read_text()) for p in sorted(CORPUS.glob("*.json"))]
    groups: dict[tuple[str, str], list[dict]] = {}

    for doc in docs:
        pdf = CORPUS / f"{doc['name']}.pdf"
        family = re.sub(r"_\d+$", "", doc["name"])
        extractors = text_ex if doc["kind"] == "reading_order" else table_ex
        for name, fn in extractors.items():
            t0 = time.perf_counter()
            try:
                result = fn(pdf)
                s = (score_reading(doc["lines"], result) if doc["kind"] == "reading_order"
                     else score_table(doc["table"], result))
            except Exception as e:  # keep going, report the failure
                s = {"error": 1.0}
                print(f"  ! {name} on {doc['name']}: {e}")
            s["ms"] = (reported_ms[name](pdf) if name in reported_ms
                       else (time.perf_counter() - t0) * 1000)
            groups.setdefault((family, name), []).append(s)

    print(f"\n{'document family':<22} {'extractor':<26} metrics (mean over seeds)")
    print("-" * 96)
    for (family, name), rows in sorted(groups.items()):
        keys = [k for k in rows[0] if k != "ms"]
        metrics = "  ".join(f"{k}={mean(r.get(k, 0) for r in rows):.2f}" for k in keys)
        ms = mean(r["ms"] for r in rows)
        print(f"{family:<22} {name:<26} {metrics}  ({ms:.0f} ms)")


if __name__ == "__main__":
    main()
