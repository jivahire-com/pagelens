"""Evaluate parsers on private resumes without an answer key.

    .venv/bin/python bench/resumes.py [--dir data/private_resumes/extracted]

Privacy: prints only anonymous IDs (R01..) and statistics, never resume text
or file names. The ID -> file mapping is written inside the private folder.

Reference content comes from PyMuPDF's word list (geometry-based words), which
is complete but unordered. Per parser:
  recall     share of reference words present in the output (dropped text)
  precision  share of output tokens that are reference words (merged/broken
             words, garbage)
  merges     our lines that run across two side-by-side text blocks
             (columns or sidebar fused into one line); ours only
  heads      section headings found, and whether they appear in page order
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
from collections import Counter
from pathlib import Path
from statistics import mean

ROOT = Path(__file__).resolve().parent.parent
ENGINE = ROOT / "target" / "release" / "pagelens"
PRIVATE = ROOT / "data" / "private_resumes"
DOCLING_CACHE = PRIVATE / "cache" / "docling"

HEADINGS = re.compile(
    r"^(summary|profile|objective|experience|work experience|professional experience|employment|"
    r"education|skills|technical skills|projects|certifications|achievements|awards|languages|"
    r"publications|interests|contact)\b", re.I)

TOKEN = re.compile(r"[A-Za-z0-9][A-Za-z0-9+#.\-/@]*[A-Za-z0-9+#]|[A-Za-z0-9]")


def tokens(text: str) -> list[str]:
    return [t.lower() for t in TOKEN.findall(text)]


def reference(path: Path) -> tuple[Counter, list, int]:
    import fitz
    words: Counter = Counter()
    blocks = []
    with fitz.open(path) as doc:
        pages = len(doc)
        for pno, page in enumerate(doc):
            for w in page.get_text("words"):
                words.update(tokens(w[4]))
            for b in page.get_text("blocks"):
                if b[6] == 0 and b[4].strip():
                    blocks.append((pno, b[0], b[1], b[2], b[3]))
    return words, blocks, pages


def ours(path: Path) -> dict:
    out = subprocess.run([str(ENGINE), str(path)], capture_output=True, text=True)
    return json.loads(out.stdout) if out.returncode == 0 else {"pages": [], "text": ""}


def pymupdf_text(path: Path) -> str:
    import fitz
    with fitz.open(path) as doc:
        return "\n".join(p.get_text("text") for p in doc)


def content_scores(ref: Counter, text: str) -> tuple[float, float]:
    got = Counter(tokens(text))
    if not ref:
        return float("nan"), float("nan")
    hit = sum(min(c, got[w]) for w, c in ref.items())
    recall = hit / sum(ref.values())
    precision = hit / max(1, sum(got.values()))
    return recall, precision


def fused_lines(result: dict, blocks: list) -> int:
    """Lines whose box spans two x-disjoint reference blocks on the same row."""
    n = 0
    for pno, page in enumerate(result.get("pages", [])):
        pb = [b for b in blocks if b[0] == pno]
        for s in page["segments"]:
            b = s["bbox"]
            hits = [x for x in pb if x[1] < b["x1"] and x[3] > b["x0"] and x[2] < b["y1"] and x[4] > b["y0"]]
            if any(hits[i][3] <= hits[j][1] or hits[j][3] <= hits[i][1]
                   for i in range(len(hits)) for j in range(i + 1, len(hits))):
                n += 1
    return n


def headings_in_order(text: str) -> tuple[int, bool]:
    found = [m.group(1).lower() for line in text.splitlines() if (m := HEADINGS.match(line.strip()))
             and len(line.strip()) < 40]
    return len(found), len(found) == len(set(found))


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--set", default="synthetic", choices=["synthetic", "real"])
    args = ap.parse_args()
    src, prefix, id_file, cache = {
        "synthetic": ("extracted", "R", "id_map.json", "docling"),
        "real": ("real", "X", "id_map_real.json", "docling_real"),
    }[args.set]
    global DOCLING_CACHE
    DOCLING_CACHE = PRIVATE / "cache" / cache
    base = PRIVATE / src
    pdfs = sorted(base.rglob("*.pdf"))
    ids = {p: f"{prefix}{i + 1:02d}" for i, p in enumerate(pdfs)}
    (PRIVATE / id_file).write_text(json.dumps({v: str(k.relative_to(base)) for k, v in ids.items()}, indent=1))

    rows = []
    for p in pdfs:
        ref, blocks, pages = reference(p)
        r = ours(p)
        row = {"id": ids[p], "pages": pages, "ref_words": sum(ref.values())}
        row["ours"] = content_scores(ref, r.get("text", ""))
        row["ours_fused"] = fused_lines(r, blocks)
        row["ours_heads"] = headings_in_order(r.get("text", ""))
        row["pymupdf"] = content_scores(ref, pymupdf_text(p))
        cache = DOCLING_CACHE / f"{ids[p]}.json"
        if cache.exists():
            d = json.loads(cache.read_text())
            row["docling"] = content_scores(ref, d["text"])
            row["docling_heads"] = headings_in_order(d["text"])
        rows.append(row)

    scanned = [r["id"] for r in rows if r["ref_words"] < 20]
    print(f"{len(rows)} resumes, {sum(r['pages'] for r in rows)} pages; "
          f"no text layer (need OCR): {len(scanned)} {scanned}")
    print(f"\n{'id':<5}{'pages':>6}{'words':>7}  {'ours rec/prec':>14} {'fused':>6}  "
          f"{'pymupdf rec/prec':>17}  {'docling rec/prec':>17}")
    for r in rows:
        dl = r.get("docling")
        dl_s = f"{dl[0]:.3f}/{dl[1]:.3f}" if dl else "-"
        print(f"{r['id']:<5}{r['pages']:>6}{r['ref_words']:>7}  {r['ours'][0]:.3f}/{r['ours'][1]:.3f} "
              f"{r['ours_fused']:>6}  {r['pymupdf'][0]:>8.3f}/{r['pymupdf'][1]:.3f}  {dl_s:>17}")

    ok = [r for r in rows if r["ref_words"] >= 20]
    def avg(key, i):
        vals = [r[key][i] for r in ok if key in r]
        return mean(vals) if vals else float("nan")
    print(f"\nmean over {len(ok)} resumes with text:")
    for key in ("ours", "pymupdf", "docling"):
        print(f"  {key:<8} recall={avg(key, 0):.3f}  precision={avg(key, 1):.3f}")
    print(f"  ours: resumes with fused column lines = {sum(r['ours_fused'] > 0 for r in ok)}, "
          f"total fused lines = {sum(r['ours_fused'] for r in ok)}")


if __name__ == "__main__":
    main()
