"""Quantise the DistilBERT-NER ONNX model to int8 (about 260MB -> 63MB).

    python3 scripts/quantize_ner.py model.onnx model.int8.onnx

Dynamic int8 quantisation of the weights; predictions match the fp32 model
on our checks. Requires: pip install onnx onnxruntime
"""
import sys

from onnxruntime.quantization import QuantType, quantize_dynamic

src, dst = sys.argv[1], sys.argv[2]
quantize_dynamic(src, dst, weight_type=QuantType.QInt8)
print(f"wrote {dst}")
