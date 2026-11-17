#!/bin/sh
# Run every accuracy check: synthetic, private resumes (if present), DP-Bench.
cd "$(dirname "$0")/.."
echo "== synthetic (non-perfect rows only)"
.venv/bin/python bench/evaluate.py --engine target/release/pagelens --only ours 2>&1 \
  | grep -v "1.00  order=1.00  similarity=1.00\|row_match=1.00  shape=1.00" | tail -n +4
[ -f data/private_resumes/id_map.json ] && echo "== resumes" && .venv/bin/python bench/resume_agreement.py
echo "== DP-Bench"
.venv/bin/python bench/dpbench.py --ours-only | tail -n 2
