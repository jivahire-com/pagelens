"""Blind judging via Claude Code sub-agents (no API key needed).

    .venv/bin/python bench/judge_blind.py prepare   # anonymised copies + key
    .venv/bin/python bench/judge_blind.py score     # scorecard from verdicts

prepare: copies each sample's page image and parser outputs into
  bench/judge/blind/<id>/{page.png, A.txt, B.txt, C.txt}
with a shuffled, per-page letter order. The letter -> parser key goes to
bench/judge/blind_key.json, outside the folder the judges read.

Judges (sub-agents) write bench/judge/verdicts/<id>.json in the schema in
JUDGE_SCHEMA below. score: maps letters back to parsers and prints mean
scores per dimension, wins, escalations, and our listed errors.
"""
from __future__ import annotations

import json
import os
import random
import shutil
import sys
from pathlib import Path
from statistics import mean

ROOT = Path(__file__).resolve().parent.parent
J = Path(os.environ.get("JUDGE_DIR", ROOT / "bench" / "judge"))  # private sets live elsewhere
SAMPLES, BLIND, VERDICTS, KEY = J / "samples", J / "blind", J / "verdicts", J / "blind_key.json"
SYSTEMS = {"ours": "ours.txt", "docling": "docling.txt", "pymupdf": "pymupdf.txt"}
DIMENSIONS = ["text_completeness", "reading_order", "columns", "tables", "formatting"]

JUDGE_SCHEMA = {
    "sample": "<id>",
    "judge_model": "haiku|opus",
    "page_has_tables": "true|false",
    "page_layout": "single_column|multi_column|sidebar|mixed|table_heavy",
    "outputs": [{"label": "A", **{d: "1-5 (tables: 0 if no table)" for d in DIMENSIONS},
                 "errors": [{"dimension": "...", "description": "...", "evidence": "short quote"}]}],
    "best_overall": "A|B|C|tie",
    "confidence": "0.0-1.0",
}


def prepare() -> None:
    if BLIND.exists():
        shutil.rmtree(BLIND)
    key = {}
    for s in sorted(p for p in SAMPLES.iterdir() if p.is_dir()):
        order = list(SYSTEMS)
        random.Random(f"blind-{s.name}").shuffle(order)
        d = BLIND / s.name
        d.mkdir(parents=True)
        shutil.copy(s / "page.png", d / "page.png")
        for label, system in zip("ABC", order):
            shutil.copy(s / SYSTEMS[system], d / f"{label}.txt")
        key[s.name] = dict(zip("ABC", order))
    KEY.write_text(json.dumps(key, indent=1))
    VERDICTS.mkdir(parents=True, exist_ok=True)
    print(f"blinded {len(key)} samples into {BLIND}")


def score() -> None:
    key = json.loads(KEY.read_text())
    rows = []
    for f in sorted(VERDICTS.glob("*.json")):
        v = json.loads(f.read_text(), strict=False)  # judges may leave raw tabs in quotes
        sid = v.get("sample", f.stem)
        m = key[sid]
        scores = {m[o["label"][:1]]: o for o in v["outputs"] if o["label"][:1] in m}
        best = m.get(str(v["best_overall"])[:1], "tie")
        rows.append((sid, v, scores, best))

    print(f"{len(rows)} pages judged; escalated to Opus: "
          f"{sum(1 for _, v, _, _ in rows if 'opus' in str(v.get('judge_model', '')).lower())}")
    print(f"\n{'parser':<10}" + "".join(f"{d[:13]:>15}" for d in DIMENSIONS) + f"{'wins':>6}")
    for system in SYSTEMS:
        line = f"{system:<10}"
        for d in DIMENSIONS:
            vals = [int(sc[system][d]) for _, v, sc, _ in rows if system in sc and d in sc[system]
                    and (d != "tables" or str(v.get("page_has_tables")).lower() == "true")]
            line += f"{mean(vals):>15.2f}" if vals else f"{'-':>15}"
        line += f"{sum(b == system for *_, b in rows):>6}"
        print(line)

    print("\nper page (ours / docling / pymupdf, mean of dimensions; * = best):")
    for sid, v, sc, best in rows:
        def avg(s):
            dims = [d for d in DIMENSIONS if d != "tables" or str(v.get("page_has_tables")).lower() == "true"]
            got = [int(sc[s][d]) for d in dims if s in sc and d in sc[s]]
            return mean(got) if got else float("nan")
        cells = " ".join(f"{avg(s):.1f}{'*' if best == s else ' '}" for s in SYSTEMS)
        print(f"  {sid:<9} {cells}   conf={v.get('confidence')} judge={v.get('judge_model')}")

    print("\nour errors (what to fix):")
    for sid, v, sc, _ in rows:
        for e in sc.get("ours", {}).get("errors", []):
            print(f"  {sid:<9} [{e['dimension']}] {e['description']}")


if __name__ == "__main__":
    {"prepare": prepare, "score": score}[sys.argv[1]]()
