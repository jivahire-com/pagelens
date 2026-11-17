"""Resume reading-order agreement with Docling (statistics only, no text)."""
import json, subprocess, sys, statistics as st
sys.path.insert(0, __import__("os").path.dirname(__file__))
from resumes import ENGINE, PRIVATE, tokens
import fitz

ids = json.loads((PRIVATE / "id_map.json").read_text())
def grams(t, n=4):
    w = tokens(t); return {tuple(w[i:i + n]) for i in range(len(w) - n + 1)}
rows, ntab = [], 0
for rid, rel in sorted(ids.items()):
    p = PRIVATE / "extracted" / rel
    r = json.loads(subprocess.run([str(ENGINE), str(p)], capture_output=True, text=True).stdout)
    ntab += sum(len(pg["tables"]) for pg in r["pages"])
    dl = json.loads((PRIVATE / "cache" / "docling" / f"{rid}.json").read_text())["text"]
    with fitz.open(p) as d: mu = "\n".join(pg.get_text() for pg in d)
    go, gd, gm = grams(r["text"]), grams(dl), grams(mu)
    f = lambda a, b: len(a & b) / max(1, len(a | b))
    rows.append((f(go, gd), f(gm, gd), rid))
print(f"tables {ntab} | agreement with docling: ours {st.mean(r[0] for r in rows):.3f}, pymupdf {st.mean(r[1] for r in rows):.3f} "
      f"| ours worse: {sum(r[0] < r[1] - 0.02 for r in rows)}, better: {sum(r[0] > r[1] + 0.02 for r in rows)}")
