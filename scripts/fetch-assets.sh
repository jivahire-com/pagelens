#!/bin/sh
# Download the assets pagelens compiles into its binary:
#   vendor/pdfium/        PDFium shared library for this platform (BSD-3-Clause)
#   models/ocrs/          ocrs text detection and recognition models
#   models/ner/           DistilBERT-NER (Apache-2.0), quantised to int8
# The fonts in models/fonts/ are committed (SIL Open Font License).
#
# Usage: scripts/fetch-assets.sh [--no-ner]
# NER quantisation needs Python 3.9+ with `pip install onnx onnxruntime`.
set -eu
cd "$(dirname "$0")/.."

PDFIUM_RELEASE="chromium/8152"
OCRS_BASE="https://ocrs-models.s3-accelerate.amazonaws.com"
NER_REPO="https://huggingface.co/dslim/distilbert-NER/resolve/main/onnx"

# PAGELENS_PLATFORM overrides detection when cross-compiling
# (mac-x64, mac-arm64, linux-x64, linux-arm64, win-x64, win-arm64); the
# library then goes to vendor/pdfium-<platform>.
if [ -n "${PAGELENS_PLATFORM:-}" ]; then
  PLATFORM=$PAGELENS_PLATFORM
  PDFIUM_DIR=vendor/pdfium-$PLATFORM
else
  case "$(uname -s)-$(uname -m)" in
    Darwin-x86_64) PLATFORM=mac-x64 ;;
    Darwin-arm64) PLATFORM=mac-arm64 ;;
    Linux-x86_64) PLATFORM=linux-x64 ;;
    Linux-aarch64 | Linux-arm64) PLATFORM=linux-arm64 ;;
    MINGW*-x86_64 | MSYS*-x86_64 | CYGWIN*-x86_64) PLATFORM=win-x64 ;;
    MINGW*-aarch64 | MSYS*-aarch64 | MINGW*-arm64 | MSYS*-arm64) PLATFORM=win-arm64 ;;
    *) echo "unsupported platform: $(uname -s)-$(uname -m)" >&2; exit 1 ;;
  esac
  PDFIUM_DIR=vendor/pdfium
fi

if [ ! -d "$PDFIUM_DIR/lib" ] && [ ! -d "$PDFIUM_DIR/bin" ]; then
  echo "PDFium ($PLATFORM)..."
  mkdir -p "$PDFIUM_DIR"
  curl -fsSL "https://github.com/bblanchon/pdfium-binaries/releases/download/$PDFIUM_RELEASE/pdfium-$PLATFORM.tgz" |
    tar -xz -C "$PDFIUM_DIR"
fi

mkdir -p models/ocrs
for m in text-detection.onnx text-recognition.onnx; do
  [ -f "models/ocrs/$m" ] || { echo "ocrs $m..."; curl -fsSL -o "models/ocrs/$m" "$OCRS_BASE/$m"; }
done

if [ "${1:-}" != "--no-ner" ] && [ ! -f models/ner/model.int8.onnx ]; then
  mkdir -p models/ner
  echo "NER model..."
  curl -fsSL -o models/ner/tokenizer.json "$NER_REPO/tokenizer.json"
  curl -fsSL -o models/ner/config.json "$NER_REPO/config.json"
  curl -fsSL -o models/ner/model.onnx "$NER_REPO/model.onnx"
  PY=${PYTHON:-$(command -v python3 || command -v python)}
  "$PY" scripts/quantize_ner.py models/ner/model.onnx models/ner/model.int8.onnx
  rm -f models/ner/model.onnx
fi
echo "assets ready. Build: cargo build --release"
echo "(with built-in OCR and the name model: cargo build --release --features ocr,ner)"
