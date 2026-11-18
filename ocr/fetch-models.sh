#!/bin/sh
# Download the PP-OCRv6 tiny models pagelens-ocr compiles into its binary
# (PaddleOCR, Apache-2.0; ONNX conversions by RapidAI/RapidOCR, Apache-2.0)
# into models/ppocr/ at the repository root, and check their SHA-256.
#
# Usage: ocr/fetch-models.sh
set -eu
cd "$(dirname "$0")/.."

BASE="https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.10.0/onnx/PP-OCRv6"
DIR=models/ppocr
mkdir -p "$DIR"

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

fetch() { # kind file sha256
  if [ ! -f "$DIR/$2" ] || [ "$(sha256 "$DIR/$2")" != "$3" ]; then
    echo "PP-OCRv6 $2..."
    curl -fsSL -o "$DIR/$2.part" "$BASE/$1/$2"
    got=$(sha256 "$DIR/$2.part")
    if [ "$got" != "$3" ]; then
      rm -f "$DIR/$2.part"
      echo "checksum mismatch for $2 (got $got)" >&2
      exit 1
    fi
    mv "$DIR/$2.part" "$DIR/$2"
  fi
}

fetch det PP-OCRv6_det_tiny.onnx f42c0fbd294d95eac1a550e131b277dac97462c8025fa4b6c3cec1b7894bd3d5
fetch rec PP-OCRv6_rec_tiny.onnx e16e242de5937ad92609223f19bc2aff3727ee40b095f996907c24749bad251b
echo "models ready. Build: cd ocr && cargo build --release"
