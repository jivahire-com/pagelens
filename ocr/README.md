# pagelens-ocr

An optional companion to pagelens that reads scanned pages and text in images on your own
machine with PP-OCRv6, then runs the pagelens parser on the result. Its output is the same JSON
as pagelens. It is pure Rust and needs no Python, no C++ runtime and no GPU. The OCR models are
compiled into the binary, so nothing is downloaded at run time and nothing leaves the machine.

How to build it, how to use it, its output, and how to limit its CPU use are in the main
[README](../README.md#scanned-documents). This page covers accuracy, limitations and the models.

## Accuracy and cost

Measured on 20 resume pages, each turned into an image-only PDF with
JPEG compression (a typical office scan) and compared with the same page's
text layer:

| | pagelens-ocr (PP-OCRv6 tiny) | PaddleOCR-VL 1.6 (0.9B vision model) |
|---|---|---|
| Word accuracy (F1) | 0.981 | 0.993 |
| Emails read exactly | 19/20 | 17/20 |
| Phones read exactly | 19/19 | 18/19 |
| Reading order, compared with pagelens on the same page's digital version | 0.966 | not comparable (plain text, no layout) |
| Time per page (laptop CPU, all cores) | about 2 s | about 110 s |
| Peak memory | about 400 MB | about 2.3 GB |
| Size | 25 MB binary, models built in | 44 MB binary, plus a 1.8 GB model |

The vision model reads running text slightly better. On scans it sometimes
skipped the small contact line under the name; PP-OCRv6 didn't.

## Limitations

- **Tables with border lines.** On a scan the lines are pixels, not
  drawing commands, so pagelens doesn't see a table there and reads it as
  lines of text.
- **Upright text only.** Rotated pages and vertical text aren't handled.
- **No handwriting.** The models aren't trained for it.
- **Phone photos untested.** Results on photographed documents have not
  been measured.

## Models

- **PP-OCRv6 tiny,** text detection and recognition, from
  [PaddleOCR](https://github.com/PaddlePaddle/PaddleOCR) (Apache-2.0). The
  ONNX conversions come from [RapidOCR](https://github.com/RapidAI/RapidOCR)
  (Apache-2.0).
- **Engine.** The models run on [rten](https://github.com/robertknight/rten),
  the same engine pagelens's optional `ocr` feature uses.
- **Pipeline.** Pre- and post-processing follow RapidOCR's PP-OCRv6 pipeline.
