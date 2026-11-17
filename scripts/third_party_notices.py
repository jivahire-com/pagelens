"""Regenerate THIRD_PARTY_NOTICES.md from `cargo metadata`.

    cargo metadata --format-version 1 | python3 scripts/third_party_notices.py
"""
import json
import sys
from collections import defaultdict

m = json.load(sys.stdin)
nodes = {n["id"]: n for n in m["resolve"]["nodes"]}
seen, stack = set(), [m["resolve"]["root"]]
while stack:
    i = stack.pop()
    if i in seen:
        continue
    seen.add(i)
    for d in nodes[i]["deps"]:
        if any(k.get("kind") in (None, "build") for k in d["dep_kinds"]):
            stack.append(d["pkg"])
pkgs = {p["id"]: p for p in m["packages"]}
by = defaultdict(list)
for i in seen:
    p = pkgs[i]
    if p["name"] != "pagelens":
        by[p.get("license") or "UNKNOWN"].append(f"{p['name']} {p['version']}")

out = [
    "# Third-party notices", "",
    "pagelens is licensed under the Apache License 2.0 (see LICENSE). It bundles or links the components below, each under its own licence.", "",
    "## Bundled assets (compiled into the binary)", "",
    "| Component | Licence | Source | Notes |", "|---|---|---|---|",
    "| PDFium | BSD-3-Clause (plus bundled third-party code under their own licences) | https://pdfium.googlesource.com/pdfium/ via https://github.com/bblanchon/pdfium-binaries | Licence texts arrive in `vendor/pdfium/LICENSE` and `vendor/pdfium/licenses/` with `scripts/fetch-assets.sh`; ship them with binary releases. |",
    "| Noto Sans, Noto Sans Devanagari | SIL Open Font License 1.1 | https://github.com/notofonts | Text in `models/fonts/OFL.txt`. Embedded in the binary and in PDFs it writes; not to be sold on their own. |",
    "| DistilBERT-NER (dslim/distilbert-NER), int8-quantised by `scripts/quantize_ner.py` | Apache-2.0 | https://huggingface.co/dslim/distilbert-NER | Modified (quantised) model. |",
    "| ocrs text detection and recognition models | **To confirm before a public release**: the ocrs-models repository declares no licence; the models are trained on HierText (CC-BY-SA 4.0) | https://github.com/robertknight/ocrs-models | Downloaded at build time, not committed. |",
    "", "## Rust crates (default build)", "",
    f"Generated from `cargo metadata`: {len(seen) - 1} crates. Licence texts are in each crate's source; regenerate this list (and full texts, for example with `cargo about`) for each binary release.", "",
]
for lic in sorted(by):
    out += [f"### {lic}", "", ", ".join(sorted(by[lic])), ""]
open("THIRD_PARTY_NOTICES.md", "w").write("\n".join(out))
print({k: len(v) for k, v in sorted(by.items())})
