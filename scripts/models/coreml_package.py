"""Helpers that make a saved Core ML package the same bytes in every
conversion (shared by the convert_*_coreml.py scripts)."""
import json
import uuid
from pathlib import Path

MEMBERS = {
    "Manifest.json": "Manifest.json",
    "model.mlmodel": "Data/com.apple.CoreML/model.mlmodel",
    "weight.bin": "Data/com.apple.CoreML/weights/weight.bin",
}


def fix_manifest(path: Path) -> None:
    """coremltools names the package's items by random UUIDs; name them by
    their path instead, so two conversions give the same bytes."""
    manifest = json.loads(path.read_text())
    renamed = {}
    root = manifest["rootModelIdentifier"]
    for key, item in manifest["itemInfoEntries"].items():
        new = str(uuid.uuid5(uuid.NAMESPACE_URL, "rawtonic:" + item["path"])).upper()
        renamed[new] = item
        if key == root:
            manifest["rootModelIdentifier"] = new
    manifest["itemInfoEntries"] = dict(sorted(renamed.items()))
    path.write_text(json.dumps(manifest, indent=4) + "\n")


def sort_spec(path: Path, conversion_date: str) -> None:
    """The model's protobuf holds maps (metadata, operation arguments) that
    are written in hash order; write them sorted (same bytes every time),
    with the conversion date fixed."""
    from coremltools.proto import Model_pb2

    spec = Model_pb2.Model()
    spec.ParseFromString(path.read_bytes())
    metadata = spec.description.metadata.userDefined
    if "com.github.apple.coremltools.conversion_date" in metadata:
        metadata["com.github.apple.coremltools.conversion_date"] = conversion_date
    path.write_bytes(spec.SerializeToString(deterministic=True))
