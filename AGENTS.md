# Notes for coding agents

pagelens is a Rust document parser. It ships as a single binary with PDFium
and the fonts built in. OCR (`ocr`) and the name model (`ner`) are opt-in
build features and are off by default. Scans and text images are listed in
`needs_review` instead of being read.

`ocr/` is a separate, optional crate (`pagelens-ocr`) that reads those scans
locally with PP-OCRv6 and then runs pagelens on them. Build it from `ocr/`
(`cargo build --release`, after `ocr/fetch-models.sh`) and test it with
`cargo test` there. It depends on the parser crate but doesn't change it.

## Before and after every change

- Run `cargo build --release`, then `cargo test`, then `bench/check_all.sh`.
- `bench/check_all.sh` covers the synthetic documents, the resumes and
  DP-Bench. A change that lowers any of these scores is not finished.
- Fixes must be generic and target a kind of document, never one particular
  file. Tests use synthetic documents only.

## Personal data

- `data/` is git-ignored and may hold real resumes. Never commit anything
  from it, never use `git add -f` on it, and never print, quote or paste its
  contents.
- Reports are counts and percentages only, and refer to files by id.
- Credentials are set by the owner as environment variables in their own
  terminal. Never ask for them in chat and never write them to a file.
