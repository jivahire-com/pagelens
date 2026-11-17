# pagelens

**Fast, offline document parser.** Give it a PDF, Word, PowerPoint or HTML file. You get back
clean Markdown, the personal data it found, and a list of what it couldn't read.

- One ~16 MB binary with PDFium built in. No internet, no GPU, no Python.
- No machine learning in the default build, so the same file always gives the same result.
- Works out reading order from the page layout, across columns, sidebars and coloured panels.
- Tables: ruled, striped, rule-separated and borderless.
- Personal data: emails, phones, names, addresses and ID numbers, each with its type, position
  and confidence. Also a redacted copy that is safe to send to an AI service.
- `needs_review` lists the scanned pages and images with text in them, so only those go to OCR
  or a vision model.

> **Status:** early release (0.1). Interfaces may change.

## Use cases

| Use case | What you get |
|---|---|
| Resume and document extraction with an LLM | Clean Markdown for the model, plus the person's name, email and phone with confidence |
| Sending documents to an AI without leaking personal data | `markdown_redacted`, with `[EMAIL]`, `[PHONE]`, `[PERSON_NAME]` ... in place of the data; city and country kept |
| Anonymised, branded CVs | `pagelens render`: one layout, your logo, the name replaced by a label, checked for leftover personal data |
| Tables from reports and papers | Rows and columns, including merged cells |
| RAG and search indexing | The document split into sections under their headings |
| Catching hidden text | White-on-white and invisible text reported in `hidden_text` and kept out of the Markdown |

**Not for:** scans or handwriting in the default build. It lists them for OCR instead; see
[Optional features](#optional-features).

## Build

You need [Rust](https://rustup.rs) (stable), `curl` and `tar`. On Windows, use Git Bash.

```bash
git clone https://github.com/jivahire-com/pagelens.git
cd pagelens
scripts/fetch-assets.sh --no-ner   # downloads PDFium
cargo build --release              # -> target/release/pagelens
```

The binary is self-contained. Copy it to any machine with the same OS and CPU type.

## Use

```bash
pagelens resume.pdf > out.json             # JSON with the Markdown inside
pagelens report.docx slides.pptx page.html # one JSON line per file
pagelens --redact resume.pdf               # no raw personal data anywhere in the output
pagelens profile resume.pdf                # the CV as standard sections (experience, skills ...)
pagelens render resume.pdf clean.pdf --logo logo.png --candidate-id "Candidate #1042"
pagelens render resume.pdf out.pdf --theme agency.json --notes notes.json --known candidate.json
```

`render` reads back every file it writes. If any personal data is still in the file, it deletes
the file and exits with code 3. See `examples/agency/` for a theme file.

From Python, or any other language, run the binary and read its JSON:

```python
import json, subprocess

doc = json.loads(subprocess.run(["pagelens", "resume.pdf"], capture_output=True, check=True).stdout)
text = doc["markdown_redacted"]                    # send this to your LLM
email = (doc["person"]["email"] or {}).get("text")
for item in doc["needs_review"]:                   # scans, text images, doubtful values
    ...
```

## Output

| Field | Contents |
|---|---|
| `markdown`, `markdown_redacted` | The document as Markdown, as written and with personal data replaced |
| `pages[].blocks[]` | Typed blocks (heading, paragraph, list item, table), each with a `bbox` and its `pii[]` |
| `sections[]` | Content grouped under each heading |
| `pii[]` | Every finding: `kind`, `text`, page, block and `confidence` |
| `person` | `name`, `email` and `phone`, each with a `confidence` and a `basis` |
| `needs_review[]` | What wasn't read, with a `reason` and an `action` (`read_with_llm`, `confirm_with_llm` ...) |
| `hidden_text` | Text in the file that a reader can't see |

## Personal data

pagelens finds:

- emails, phone numbers, profile links, dates of birth and UPI IDs;
- labelled fields such as Name, Passport and Bank Account;
- names after a title such as Mr or Dr, and the names of referees;
- ID numbers, each confirmed by its check digit or format: Aadhaar, PAN, SSN, voter ID, driving
  licence, card numbers and IBAN;
- addresses, split into the street (`address`), the PIN or ZIP code (`postal_code`) and the
  city, state and country (`location`). `location` is never redacted.

A value found once is tagged everywhere it appears.

## Optional features

```bash
scripts/fetch-assets.sh                     # also prepares the name model (needs Python 3.9+ with onnx and onnxruntime)
cargo build --release --features ocr,ner    # binary of about 100 MB
```

- `ocr`: scans and images with text in them are read instead of listed.
- `ner`: also finds names and places in running text (`--ner`).

| Setting | Values |
|---|---|
| `PAGELENS_OCR` | `auto` (default), `builtin`, `tesseract`, `ocrs`, `off` |
| `PAGELENS_NER` | `on`, `off` |
| `PAGELENS_CACHE_DIR` | Where the built-in PDFium is unpacked |
| `PDFIUM_LIB_DIR` | Use an external PDFium instead of the built-in one |

## Limitations

- Scans are listed, not read, unless you build with `ocr`.
- Word files:
  - page headers and footers aren't read;
  - link addresses are dropped;
  - a text box stored with a fallback copy can appear twice.

  Converting the file to PDF first avoids all three.
- Unusual layouts can still be read in the wrong order.

## Benchmark

[DP-Bench](https://huggingface.co/datasets/upstage/dp-bench): 200 pages, scored with the
dataset's own scorer. The other rows are the results that ship with the dataset.

| System | Text and reading order | Table detection F1 | TEDS | TEDS-S |
|---|---|---|---|---|
| **pagelens (no ML)** | 0.962 | **0.916** | 0.907 | 0.929 |
| upstage_enhanced | **0.966** | 0.898 | 0.956 | **0.976** |
| aws | 0.960 | 0.690 | 0.955 | 0.970 |
| unstructured | 0.918 | 0.887 | 0.803 | 0.895 |
| llamaparse | 0.905 | 0.548 | 0.907 | 0.932 |
| microsoft | 0.870 | 0.850 | 0.779 | 0.857 |
| google | 0.822 | 0.835 | 0.783 | 0.807 |

TEDS scores table structure and content; TEDS-S scores structure only. pagelens takes a median
of 38 ms per page on 2 CPU cores, including process start.

## Contributing

Run `cargo test` and `bench/check_all.sh` before you send a change. A change that lowers a
score isn't finished. Fix a kind of document, proven with a synthetic fixture in
`tests/fixtures`, never one particular file. Never commit real documents or personal data.

## Licence

Apache 2.0. See `LICENSE`, `NOTICE` and `THIRD_PARTY_NOTICES.md`.
