"""Blind vision-LLM judge for PDF parser outputs.

    .venv-judge/bin/python bench/judge.py [--limit N] [--threshold 0.7]

For every sample in bench/judge/samples/<id>/ the judge sees the page image
and each parser's output under a shuffled letter (A/B/C), and scores each on
text completeness, reading order, columns, tables and formatting, listing
concrete errors with evidence. Claude Haiku 5.5 judges first; if it reports
confidence below the threshold, Claude Opus 5.5 judges again and its verdict
is used.

Writes bench/judge/results/<id>.json and prints a scorecard per parser.
Credentials come from the environment (ANTHROPIC_API_KEY or `ant auth login`).
"""
from __future__ import annotations

import argparse
import base64
import json
import random
import sys
from pathlib import Path
from statistics import mean

import anthropic

ROOT = Path(__file__).resolve().parent.parent
SAMPLES = ROOT / "bench" / "judge" / "samples"
RESULTS = ROOT / "bench" / "judge" / "results"
SYSTEMS = {"ours": "ours.txt", "docling": "docling.txt", "pymupdf": "pymupdf.txt"}
CHEAP, STRONG = "claude-haiku-5-5", "claude-opus-5-5"
# USD per million tokens (input, output), from the published price list.
PRICES = {CHEAP: (0.10, 0.50), STRONG: (4.00, 20.00)}
DIMENSIONS = ["text_completeness", "reading_order", "columns", "tables", "formatting"]

INSTRUCTIONS = """You are judging how accurately different PDF parsers extracted this page.
The image shows the true page. Below it are the outputs of several parsers,
labelled with letters. The labels are random; judge only the content.

Score every output on each dimension from 1 (very poor) to 5 (perfect):
- text_completeness: all visible text is present, nothing invented, words not
  merged or split, characters correct.
- reading_order: text follows the order a person would read the page.
- columns: separate columns, sidebars and boxes are kept apart, never
  interleaved line by line.
- tables: tables are recognised and their rows and cells are correct. Use 0
  for every output if the page has no table.
- formatting: headings, lists and paragraphs are recognisable as such.
Ignore differences in markup style (Markdown symbols, "|" separators, blank
lines) and in hyphenation; judge the content and structure.

List each output's concrete errors (most important first, at most 6), each
with the dimension and a short quote from the output as evidence.
Then pick the best output overall, and give your confidence (0 to 1) that
your scores and ranking are right. Use low confidence when the image is hard
to read or the outputs are too close to call."""

SCHEMA = {
    "type": "object",
    "properties": {
        "page_has_tables": {"type": "boolean"},
        "page_layout": {"type": "string", "enum": ["single_column", "multi_column", "sidebar", "mixed", "table_heavy"]},
        "outputs": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "label": {"type": "string"},
                    **{d: {"type": "integer"} for d in DIMENSIONS},
                    "errors": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "dimension": {"type": "string", "enum": DIMENSIONS},
                                "description": {"type": "string"},
                                "evidence": {"type": "string"},
                            },
                            "required": ["dimension", "description", "evidence"],
                            "additionalProperties": False,
                        },
                    },
                },
                "required": ["label", *DIMENSIONS, "errors"],
                "additionalProperties": False,
            },
        },
        "best_overall": {"type": "string"},
        "confidence": {"type": "number"},
    },
    "required": ["page_has_tables", "page_layout", "outputs", "best_overall", "confidence"],
    "additionalProperties": False,
}


def build_content(sample: Path, order: list[str]) -> list[dict]:
    image = base64.standard_b64encode((sample / "page.png").read_bytes()).decode()
    content: list[dict] = [
        {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": image}},
        {"type": "text", "text": INSTRUCTIONS},
    ]
    for label, system in zip("ABC", order):
        text = (sample / SYSTEMS[system]).read_text()
        content.append({"type": "text", "text": f"<output label=\"{label}\">\n{text}\n</output>"})
    return content


def ask(client: anthropic.Anthropic, model: str, content: list[dict]) -> tuple[dict, anthropic.types.Usage | None]:
    params = dict(
        model=model,
        max_tokens=16000,
        messages=[{"role": "user", "content": content}],
        output_config={"format": {"type": "json_schema", "schema": SCHEMA}},
    )
    if model == STRONG:
        # Server-side refusal fallback, so a decline does not lose the verdict.
        response = client.beta.messages.create(
            **params, betas=["server-side-fallback-2026-07-01"], fallbacks="default"
        )
    else:
        response = client.messages.create(**params)
    if response.stop_reason == "refusal":
        raise RuntimeError(f"{model} declined to judge")
    text = next(b.text for b in response.content if b.type == "text")
    return json.loads(text), response.usage


def cost(model: str, usage) -> float:
    if usage is None:
        return 0.0
    pin, pout = PRICES[model]
    return (usage.input_tokens * pin + usage.output_tokens * pout) / 1e6


def judge_sample(client: anthropic.Anthropic, sample: Path, threshold: float) -> dict:
    order = list(SYSTEMS)
    random.Random(sample.name).shuffle(order)  # stable per sample, hidden from the judge
    content = build_content(sample, order)
    verdict, usage = ask(client, CHEAP, content)
    record = {"sample": sample.name, "order": order, "cheap": verdict, "cost": cost(CHEAP, usage),
              "judge": CHEAP}
    if verdict.get("confidence", 0) < threshold:
        strong, usage2 = ask(client, STRONG, content)
        record.update(strong=strong, judge=STRONG)
        record["cost"] += cost(STRONG, usage2)
    final = record.get("strong", verdict)
    by_label = dict(zip("ABC", order))
    record["final"] = {
        "page_has_tables": final["page_has_tables"],
        "page_layout": final["page_layout"],
        "confidence": final["confidence"],
        "best": by_label.get(final["best_overall"].strip()[:1], "tie"),
        "scores": {by_label[o["label"].strip()[:1]]: {d: o[d] for d in DIMENSIONS} for o in final["outputs"]
                   if o["label"].strip()[:1] in by_label},
        "errors": {by_label[o["label"].strip()[:1]]: o["errors"] for o in final["outputs"]
                   if o["label"].strip()[:1] in by_label},
    }
    return record


def scorecard(records: list[dict]) -> None:
    print(f"\n{'parser':<10}" + "".join(f"{d[:12]:>14}" for d in DIMENSIONS) + f"{'wins':>7}")
    for system in SYSTEMS:
        row = f"{system:<10}"
        for d in DIMENSIONS:
            vals = [r["final"]["scores"][system][d] for r in records
                    if system in r["final"]["scores"] and (d != "tables" or r["final"]["page_has_tables"])]
            row += f"{mean(vals):>14.2f}" if vals else f"{'-':>14}"
        row += f"{sum(r['final']['best'] == system for r in records):>7}"
        print(row)
    escalated = sum(r["judge"] == STRONG for r in records)
    print(f"\n{len(records)} pages judged, {escalated} escalated to {STRONG}, "
          f"total cost ${sum(r['cost'] for r in records):.3f}")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--limit", type=int)
    ap.add_argument("--threshold", type=float, default=0.7)
    ap.add_argument("--rescore", action="store_true", help="ignore cached results")
    args = ap.parse_args()
    client = anthropic.Anthropic()
    RESULTS.mkdir(parents=True, exist_ok=True)
    samples = sorted(p for p in SAMPLES.iterdir() if p.is_dir())[: args.limit]
    records = []
    for s in samples:
        out = RESULTS / f"{s.name}.json"
        if out.exists() and not args.rescore:
            records.append(json.loads(out.read_text()))
            continue
        try:
            rec = judge_sample(client, s, args.threshold)
        except (anthropic.APIError, RuntimeError, json.JSONDecodeError, KeyError) as e:
            print(f"{s.name}: failed ({type(e).__name__}: {e})", file=sys.stderr)
            continue
        out.write_text(json.dumps(rec, indent=1))
        records.append(rec)
        f = rec["final"]
        print(f"{s.name:<10} judge={rec['judge']:<17} conf={f['confidence']:.2f} best={f['best']:<8} "
              f"${rec['cost']:.4f}", flush=True)
    if records:
        scorecard(records)


if __name__ == "__main__":
    main()
