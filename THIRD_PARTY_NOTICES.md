# Third-party notices

pagelens is licensed under the Apache License 2.0 (see LICENSE). It bundles or links the components below, each under its own licence.

## Bundled assets (compiled into the binary)

| Component | Licence | Source | Notes |
|---|---|---|---|
| PDFium | BSD-3-Clause (plus bundled third-party code under their own licences) | https://pdfium.googlesource.com/pdfium/ via https://github.com/bblanchon/pdfium-binaries | Licence texts arrive in `vendor/pdfium/LICENSE` and `vendor/pdfium/licenses/` with `scripts/fetch-assets.sh`; ship them with binary releases. |
| Noto Sans, Noto Sans Devanagari | SIL Open Font License 1.1 | https://github.com/notofonts | Text in `models/fonts/OFL.txt`. Embedded in the binary and in PDFs it writes; not to be sold on their own. |
| DistilBERT-NER (dslim/distilbert-NER), int8-quantised by `scripts/quantize_ner.py` | Apache-2.0 | https://huggingface.co/dslim/distilbert-NER | Modified (quantised) model. |
| ocrs text detection and recognition models | **To confirm before a public release**: the ocrs-models repository declares no licence; the models are trained on HierText (CC-BY-SA 4.0) | https://github.com/robertknight/ocrs-models | Downloaded at build time, not committed. |

## Rust crates (default build)

Generated from `cargo metadata`: 134 crates. Licence texts are in each crate's source; regenerate this list (and full texts, for example with `cargo about`) for each binary release.

### (MIT OR Apache-2.0) AND Unicode-3.0

unicode-ident 1.0.27, unicode-normalization 0.1.26

### 0BSD OR MIT OR Apache-2.0

adler2 2.0.1

### Apache-2.0 OR MIT

autocfg 1.5.1, bit-set 0.8.0, bit-vec 0.8.0, equivalent 1.0.3, indexmap 2.14.2, pin-project-lite 0.2.17, rustc-hash 2.1.3, zeroize 1.9.1, zeroize_derive 1.5.0

### Apache-2.0/MIT

console_error_panic_hook 0.1.7

### BSD-2-Clause OR Apache-2.0 OR MIT

zerocopy 0.8.63, zerocopy-derive 0.8.63

### BSD-3-Clause OR Apache-2.0

moxcms 0.8.1, pxfm 0.1.30

### CC0-1.0 OR MIT-0 OR Apache-2.0

constant_time_eq 0.6.1

### ISC

libloading 0.9.0

### MIT

bytes 1.12.1, cfg_aliases 0.2.2, crunchy 0.2.4, fancy-regex 0.19.2, fax 0.2.7, nix 0.31.3, piston-float 1.0.1, quick-xml 0.42.0, simd-adler32 0.3.10, slab 0.4.12, tiff 0.11.3, tokio 1.54.1, vecmath 1.0.0, zip 9.5.1, zmij 1.0.23

### MIT OR Apache-2.0

android_system_properties 0.1.6, bitflags 2.13.2, bumpalo 3.20.3, cc 1.11.1, cfg-if 1.0.5, chrono 0.4.45, core-foundation-sys 0.8.7, crc32fast 1.5.5, crossbeam-deque 0.8.8, crossbeam-epoch 0.9.21, crossbeam-utils 0.8.23, either 1.19.0, fdeflate 0.3.7, find-msvc-tools 0.1.15, flate2 1.1.11, futures-core 0.3.34, futures-task 0.3.34, futures-util 0.3.34, getrandom 0.4.3, half 2.7.1, hashbrown 0.17.1, hermit-abi 0.5.3, iana-time-zone 0.1.65, iana-time-zone-haiku 0.1.2, image 0.25.10, image-webp 0.2.4, itertools 0.15.0, itoa 1.0.19, js-sys 0.3.107, libc 0.2.191, log 0.4.35, maybe-owned 0.3.4, num-traits 0.2.19, num_cpus 1.17.0, ocrs 0.14.0, once_cell 1.21.4, pdfium-render 0.9.5, png 0.18.1, proc-macro2 1.0.107, quote 1.0.47, rayon 1.12.0, rayon-core 1.13.0, regex 1.13.1, regex-automata 0.4.18, regex-syntax 0.8.11, rten 0.28.0, rten-base 0.28.0, rten-gemm 0.28.0, rten-imageproc 0.28.0, rten-onnx 0.28.0, rten-parallel 0.28.0, rten-sequence 0.28.0, rten-shape-inference 0.28.0, rten-simd 0.28.0, rten-tensor 0.28.0, rten-text 0.28.0, rten-vecmath 0.28.0, rustversion 1.0.23, serde 1.0.229, serde_core 1.0.229, serde_derive 1.0.229, serde_json 1.0.154, shlex 2.0.1, smallvec 1.16.5, syn 2.0.119, syn 3.0.10, thiserror 2.0.21, thiserror-impl 2.0.21, typed-path 0.13.0, typeid 1.0.3, unicode_categories 0.1.1, utf16string 0.2.0, wasm-bindgen 0.2.130, wasm-bindgen-futures 0.4.80, wasm-bindgen-macro 0.2.130, wasm-bindgen-macro-support 0.2.130, wasm-bindgen-shared 0.2.130, web-sys 0.3.107, weezl 0.1.12, windows-core 0.62.2, windows-implement 0.60.2, windows-interface 0.59.3, windows-link 0.2.1, windows-result 0.4.1, windows-strings 0.5.1, windows-sys 0.61.2

### MIT OR Apache-2.0 OR LGPL-2.1-or-later

r-efi 6.0.0

### MIT OR Apache-2.0 OR Zlib

zune-core 0.5.3, zune-jpeg 0.5.15

### MIT OR Zlib OR Apache-2.0

miniz_oxide 0.8.9, miniz_oxide 0.9.1

### MIT/Apache-2.0

console_log 1.1.0, quick-error 2.0.1

### Unlicense OR MIT

aho-corasick 1.1.5, byteorder 1.5.0, byteorder-lite 0.1.0, memchr 2.8.3

### Zlib

zlib-rs 0.6.8

### Zlib OR Apache-2.0 OR MIT

bytemuck 1.25.2, tinyvec 1.13.3
