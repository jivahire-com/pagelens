"""Speed and compute benchmark: wall time, CPU time and peak memory.

    .venv/bin/python bench/speed.py [--set dpbench|resumes|both] [--docling]

Each tool runs in a fresh child process over the whole document set, so
start-up and library loading are counted once, as in a real batch job.
CPU time (user + system) and peak resident memory come from the operating
system for that child process. Single process, single core for every tool.
"""
from __future__ import annotations

import argparse
import json
import resource
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PY = str(ROOT / ".venv" / "bin" / "python")
PY_DOCLING = str(ROOT / ".venv-docling" / "bin" / "python")
ENGINE = str(ROOT / "target" / "release" / "pagelens")

# Each snippet reads file paths from argv and extracts every page.
SNIPPETS = {
    "pymupdf_text (current)": "import fitz,sys\nfor f in sys.argv[1:]:\n d=fitz.open(f)\n [p.get_text('text') for p in d]",
    "pymupdf_sort (layout)": "import fitz,sys\nfor f in sys.argv[1:]:\n d=fitz.open(f)\n [p.get_text('text',sort=True) for p in d]",
    "pymupdf + find_tables": "import fitz,sys\nfor f in sys.argv[1:]:\n d=fitz.open(f)\n for p in d:\n  p.get_text('text',sort=True); p.find_tables()",
    "pdfplumber": "import pdfplumber,sys\nfor f in sys.argv[1:]:\n with pdfplumber.open(f) as d:\n  [p.extract_text() for p in d.pages]",
    "pdfplumber + tables": "import pdfplumber,sys\nfor f in sys.argv[1:]:\n with pdfplumber.open(f) as d:\n  [(p.extract_text(), p.extract_tables()) for p in d.pages]",
    "pypdfium2_text": "import pypdfium2 as m,sys\nfor f in sys.argv[1:]:\n d=m.PdfDocument(f)\n [d[i].get_textpage().get_text_range() for i in range(len(d))]",
}
DOCLING = """
import sys
from docling.backend.pypdfium2_backend import PyPdfiumDocumentBackend
from docling.datamodel.accelerator_options import AcceleratorDevice, AcceleratorOptions
from docling.datamodel.base_models import InputFormat
from docling.datamodel.pipeline_options import PdfPipelineOptions
from docling.document_converter import DocumentConverter, PdfFormatOption
o = PdfPipelineOptions(accelerator_options=AcceleratorOptions(device=AcceleratorDevice.CPU, num_threads=1))
c = DocumentConverter(format_options={InputFormat.PDF: PdfFormatOption(pipeline_options=o, backend=PyPdfiumDocumentBackend)})
for f in sys.argv[1:]:
    c.convert(f)
"""


def pdf_sets() -> dict[str, list[Path]]:
    sets = {"dpbench": sorted((ROOT / "data" / "dp-bench" / "pdfs").glob("*.pdf"))}
    ids = ROOT / "data" / "private_resumes" / "id_map.json"
    if ids.exists():
        base = ROOT / "data" / "private_resumes" / "extracted"
        sets["resumes"] = [base / rel for rel in json.loads(ids.read_text()).values()]
    return sets


def run(cmd: list[str], env_threads: bool = True) -> tuple[float, float, float]:
    """Return wall seconds, CPU seconds and peak RSS (MB) of one child."""
    import os
    env = dict(os.environ)
    if env_threads:  # keep every tool on one core for a like-for-like comparison
        env.update(OMP_NUM_THREADS="1", MKL_NUM_THREADS="1", OPENBLAS_NUM_THREADS="1")
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    t0 = time.perf_counter()
    subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True, env=env)
    wall = time.perf_counter() - t0
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    cpu = (after.ru_utime - before.ru_utime) + (after.ru_stime - before.ru_stime)
    rss_mb = after.ru_maxrss / (1024 * 1024) if sys.platform == "darwin" else after.ru_maxrss / 1024
    return wall, cpu, rss_mb


def count_pages(files: list[Path]) -> int:
    import fitz
    return sum(len(fitz.open(f)) for f in files)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--set", default="both")
    ap.add_argument("--docling", action="store_true", help="also time Docling (slow)")
    args = ap.parse_args()
    for name, files in pdf_sets().items():
        if args.set not in ("both", name):
            continue
        pages = count_pages(files)
        paths = [str(f) for f in files]
        print(f"\n== {name}: {len(files)} files, {pages} pages (one process, one core per tool)")
        print(f"{'tool':<26}{'wall s':>8}{'ms/page':>9}{'CPU s':>8}{'peak MB':>9}")
        tools = [("pagelens (ours)", [ENGINE, *paths])]
        tools += [(k, [PY, "-c", v, *paths]) for k, v in SNIPPETS.items()]
        if args.docling:
            tools.append(("docling (CPU)", [PY_DOCLING, "-c", DOCLING, *paths]))
        for tool, cmd in tools:
            # ru_maxrss is a high-water mark over all children, so measure
            # memory in a separate interpreter per tool to keep it per tool.
            wall, cpu, _ = run(cmd)
            probe = subprocess.run(
                [PY, "-c", "import resource,subprocess,sys;subprocess.run(sys.argv[1:],stdout=subprocess.DEVNULL,"
                 "stderr=subprocess.DEVNULL);r=resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss;"
                 "print(r/1048576 if sys.platform=='darwin' else r/1024)", *cmd],
                capture_output=True, text=True)
            rss = float(probe.stdout.strip() or "nan")
            print(f"{tool:<26}{wall:>8.1f}{1000 * wall / pages:>9.1f}{cpu:>8.1f}{rss:>9.0f}", flush=True)


if __name__ == "__main__":
    main()
