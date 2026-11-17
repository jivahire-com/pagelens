"""Generate synthetic PDFs with exact ground truth.

Each document is written with reportlab at known coordinates, so the correct
reading order and table cells are known exactly. Output: corpus/<name>.pdf and
corpus/<name>.json.

Ground truth schema:
{
  "name": str,
  "kind": "reading_order" | "table",
  "lines": [str, ...],              # correct reading order (reading_order docs)
  "table": [[cell, ...], ...]       # correct grid (table docs)
}
"""
from __future__ import annotations

import json
import random
from pathlib import Path

from reportlab.lib.colors import HexColor, black
from reportlab.lib.pagesizes import A4
from reportlab.pdfgen.canvas import Canvas

OUT = Path(__file__).resolve().parent.parent / "corpus"
W, H = A4

WORDS = (
    "data pipeline latency model service cluster python rust kafka design team lead "
    "customer revenue growth hiring platform search index query cache memory storage "
    "deploy monitor alert metric budget launch partner vendor contract audit review "
    "security access token policy schema migration backup restore region network "
    "mobile web client server batch stream event queue worker scale cost speed quality"
).split()


def sentence(rng: random.Random, n: int) -> str:
    return " ".join(rng.choice(WORDS) for _ in range(n)).capitalize()


def unique_lines(rng: random.Random, count: int, words: int, used: set[str]) -> list[str]:
    out = []
    while len(out) < count:
        s = sentence(rng, words)
        if s not in used:
            used.add(s)
            out.append(s)
    return out


class Recorder:
    """Records text draws so they can be written in shuffled order.

    Real PDFs (Word, Canva, LaTeX, Chrome print) often store text in an order
    unrelated to reading order. Shuffling the content stream checks that an
    extractor reads the page geometry instead of trusting stream order.
    Graphics (rects, lines) are drawn immediately; text is deferred.
    """

    def __init__(self, c: Canvas):
        self.c = c
        self.font = ("Helvetica", 10)
        self.ops: list[tuple] = []

    def setFont(self, name, size):
        self.font = (name, size)

    def drawString(self, x, y, text):
        self.ops.append((self.font, "l", x, y, text))

    def drawRightString(self, x, y, text):
        self.ops.append((self.font, "r", x, y, text))

    def __getattr__(self, attr):  # rect, line, setFillColor ... pass through
        return getattr(self.c, attr)

    def flush(self, shuffle_seed):
        ops = list(self.ops)
        if shuffle_seed is not None:
            random.Random(shuffle_seed).shuffle(ops)
        self.c.setFillColor(black)
        for font, align, x, y, text in ops:
            self.c.setFont(*font)
            (self.c.drawString if align == "l" else self.c.drawRightString)(x, y, text)


def write_seeded(family: str, seed: int, canvas_fn, truth: dict) -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    for variant, shuffle_seed in (("", None), ("_shuffled", 1234 + seed)):
        name = f"{family}{variant}_{seed}"
        c = Canvas(str(OUT / f"{name}.pdf"), pagesize=A4)
        rec = Recorder(c)
        canvas_fn(rec)
        rec.flush(shuffle_seed)
        c.save()
        (OUT / f"{name}.json").write_text(json.dumps({"name": name, **truth}, indent=2))


# ---------------------------------------------------------------- documents

def resume_sidebar(seed: int) -> None:
    """Coloured left sidebar + right main column, full-width header on top."""
    rng = random.Random(seed)
    used: set[str] = set()
    header = [f"Candidate {seed} Name", "Senior engineer resume header line"]
    side = unique_lines(rng, 14, 3, used)
    main = unique_lines(rng, 26, 7, used)

    def draw(c: Canvas) -> None:
        c.setFont("Helvetica-Bold", 18)
        c.drawString(40, H - 50, header[0])
        c.setFont("Helvetica", 10)
        c.drawString(40, H - 68, header[1])
        c.setFillColor(HexColor("#dfe7f2"))
        c.rect(30, 40, 160, H - 130, stroke=0, fill=1)
        c.setFillColor(black)
        y = H - 110
        c.setFont("Helvetica", 9)
        for s in side:
            c.drawString(40, y, s)
            y -= 16
        y = H - 110
        c.setFont("Helvetica", 10)
        for s in main:
            c.drawString(210, y, s)
            y -= 15

    write_seeded("resume_sidebar", seed, draw,
          {"kind": "reading_order", "lines": header + side + main})


def two_column_paper(seed: int) -> None:
    """Title, two columns, a full-width spanning note, then two more columns."""
    rng = random.Random(seed)
    used: set[str] = set()
    title = [f"Paper title number {seed}"]
    a1 = unique_lines(rng, 15, 5, used)
    b1 = unique_lines(rng, 15, 5, used)
    span = [sentence(rng, 16)]
    a2 = unique_lines(rng, 12, 5, used)
    b2 = unique_lines(rng, 12, 5, used)

    def draw(c: Canvas) -> None:
        c.setFont("Helvetica-Bold", 16)
        c.drawString(50, H - 60, title[0])
        c.setFont("Times-Roman", 10)
        for col, x in ((a1, 50), (b1, 310)):
            y = H - 100
            for s in col:
                c.drawString(x, y, s)
                y -= 13
        c.setFont("Times-Italic", 10)
        c.drawString(50, H - 320, span[0])
        c.setFont("Times-Roman", 10)
        for col, x in ((a2, 50), (b2, 310)):
            y = H - 350
            for s in col:
                c.drawString(x, y, s)
                y -= 13

    write_seeded("two_column_paper", seed, draw,
          {"kind": "reading_order", "lines": title + a1 + b1 + span + a2 + b2})


def single_column(seed: int) -> None:
    """Control document: plain single column. Every tool should score ~1.0."""
    rng = random.Random(seed)
    lines = unique_lines(rng, 40, 9, set())

    def draw(c: Canvas) -> None:
        c.setFont("Helvetica", 10)
        y = H - 60
        for s in lines:
            c.drawString(50, y, s)
            y -= 15

    write_seeded("single_column", seed, draw, {"kind": "reading_order", "lines": lines})


def table(seed: int, bordered: bool) -> None:
    rng = random.Random(seed)
    cols = ["Item", "Region", "Q1", "Q2", "Q3", "Total"]
    rows = [cols]
    for i in range(10):
        q = [rng.randint(100, 9999) for _ in range(3)]
        rows.append([f"{rng.choice(WORDS)} {i}", rng.choice(["North", "South", "East West"]),
                     *[f"{v:,}" for v in q], f"{sum(q):,}"])
    xs = [50, 150, 250, 320, 390, 460, 540]
    row_h = 20

    def draw(c: Canvas) -> None:
        top = H - 100
        for r, row in enumerate(rows):
            y = top - r * row_h
            c.setFont("Helvetica-Bold" if r == 0 else "Helvetica", 9)
            for k, cell in enumerate(row):
                if k >= 2:  # numeric columns right aligned
                    c.drawRightString(xs[k + 1] - 6, y + 6, cell)
                else:
                    c.drawString(xs[k] + 4, y + 6, cell)
        if bordered:
            bottom = top - (len(rows) - 1) * row_h
            for r in range(len(rows) + 1):
                y = top + row_h - r * row_h
                c.line(xs[0], y, xs[-1], y)
            for x in xs:
                c.line(x, top + row_h, x, bottom)

    kind = "bordered" if bordered else "borderless"
    write_seeded(f"table_{kind}", seed, draw, {"kind": "table", "table": rows})


def three_column_newsletter(seed: int) -> None:
    """Three prose columns. Must be read column by column, not as a table."""
    rng = random.Random(seed)
    used: set[str] = set()
    title = [f"Newsletter issue {seed}"]
    cols = [unique_lines(rng, 30, 5, used) for _ in range(3)]

    def draw(c) -> None:
        c.setFont("Helvetica-Bold", 16)
        c.drawString(40, H - 50, title[0])
        c.setFont("Helvetica", 8)
        for col, x in zip(cols, (40, 220, 400)):
            y = H - 90
            for s in col:
                c.drawString(x, y, s)
                y -= 11

    write_seeded("three_column_newsletter", seed, draw,
                 {"kind": "reading_order", "lines": title + cols[0] + cols[1] + cols[2]})


def invoice(seed: int) -> None:
    """Address left, invoice details right, borderless line items, totals."""
    rng = random.Random(seed)
    left = ["Acme Supplies Ltd", "12 Market Road", "Pune 411001"]
    right = [f"Invoice INV-{seed:04d}", "Date 2026-11-13", "Due 2026-12-13"]
    head = ["Description", "Qty", "Unit price", "Amount"]
    items = [head]
    for i in range(6):
        q, u = rng.randint(1, 20), rng.randint(100, 5000)
        items.append([f"{rng.choice(WORDS)} {rng.choice(WORDS)} service", str(q), f"{u:,}.00", f"{q*u:,}.00"])
    total = sum(int(r[3].replace(',', '').split('.')[0]) for r in items[1:])
    note = ["Thank you for your business"]

    def draw(c) -> None:
        c.setFont("Helvetica", 10)
        for k, s in enumerate(left):
            c.drawString(40, H - 60 - 14 * k, s)
        for k, s in enumerate(right):
            c.drawRightString(555, H - 60 - 14 * k, s)
        xs_right = [None, 330, 440, 555]
        for r, row in enumerate(items):
            y = H - 160 - 18 * r
            c.setFont("Helvetica-Bold" if r == 0 else "Helvetica", 9)
            c.drawString(40, y, row[0])
            for k in (1, 2, 3):
                c.drawRightString(xs_right[k], y, row[k])
        c.setFont("Helvetica-Bold", 10)
        c.drawRightString(555, H - 160 - 18 * len(items) - 10, f"Total {total:,}.00")
        c.setFont("Helvetica", 9)
        c.drawString(40, H - 160 - 18 * len(items) - 40, note[0])

    write_seeded("invoice", seed, draw, {"kind": "table", "table": items})


def table_in_prose(seed: int) -> None:
    """Paragraphs, then a borderless table with empty cells, then paragraphs."""
    rng = random.Random(seed)
    used: set[str] = set()
    before = unique_lines(rng, 6, 12, used)
    after = unique_lines(rng, 6, 12, used)
    rows = [["Metric", "2024", "2025", "Change"]]
    for i in range(7):
        a = rng.randint(10, 999)
        b = "" if i in (2, 5) else str(rng.randint(10, 999))
        rows.append([f"{rng.choice(WORDS)} {rng.choice(WORDS)}", str(a), b, "" if not b else f"{int(b)-a:+d}"])

    def draw(c) -> None:
        c.setFont("Times-Roman", 10)
        y = H - 60
        for s in before:
            c.drawString(50, y, s); y -= 13
        y -= 15
        for r, row in enumerate(rows):
            c.setFont("Times-Bold" if r == 0 else "Times-Roman", 10)
            c.drawString(60, y, row[0])
            for k, x in ((1, 300), (2, 380), (3, 460)):
                if row[k]:
                    c.drawRightString(x, y, row[k])
            y -= 15
        y -= 15
        c.setFont("Times-Roman", 10)
        for s in after:
            c.drawString(50, y, s); y -= 13

    write_seeded("table_in_prose", seed, draw, {"kind": "table", "table": rows})


def main() -> None:
    for seed in (1, 2, 3):
        resume_sidebar(seed)
        two_column_paper(seed)
        single_column(seed)
        table(seed, bordered=True)
        table(seed, bordered=False)
        three_column_newsletter(seed)
        invoice(seed)
        table_in_prose(seed)
    print(f"wrote {len(list(OUT.glob('*.pdf')))} PDFs to {OUT}")


if __name__ == "__main__":
    main()
