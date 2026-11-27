"""Validate exact compiled-case selections for the governed Windows phase."""
import hashlib
import json
import re
from pathlib import Path


def _object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"Duplicate manifest field: {key}")
        result[key] = value
    return result


def _fields(value, expected):
    if not isinstance(value, dict) or set(value) != set(expected):
        raise ValueError(f"Expected manifest fields: {', '.join(expected)}")


def _name(value):
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9_-]*", value):
        raise ValueError("Invalid package, target or variant name")
    return value


def load(path):
    raw = path.read_bytes()
    value = json.loads(raw, object_pairs_hook=_object)
    _fields(value, ("schema_version", "variants"))
    if type(value["schema_version"]) is not int or value["schema_version"] != 1:
        raise ValueError("Unsupported case manifest version")
    variants = value["variants"]
    if not isinstance(variants, list) or not variants:
        raise ValueError("At least one compiled variant is required")
    ids = set()
    for variant in variants:
        _fields(variant, ("id", "package", "kind", "target", "features", "default_features", "cases") +
                (("companions",) if "companions" in variant else ()))
        for field in ("id", "package", "target"):
            _name(variant[field])
        if variant["id"] in ids:
            raise ValueError("Duplicate compiled variant id")
        ids.add(variant["id"])
        if variant["kind"] not in ("lib", "test", "bin"):
            raise ValueError("Only existing Rust library, integration-test and binary targets are supported")
        if type(variant["default_features"]) is not bool:
            raise ValueError("default_features must be boolean")
        features = variant["features"]
        if not isinstance(features, list) or any(
                not isinstance(feature, str) or not re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9_+?/.-]*", feature)
                for feature in features) or len(set(features)) != len(features):
            raise ValueError("features must be distinct Cargo feature names")
        cases = variant["cases"]
        if not isinstance(cases, list) or not cases:
            raise ValueError("Every compiled variant needs exact cases")
        names = set()
        for case in cases:
            _fields(case, ("name", "assertions"))
            if not isinstance(case["name"], str) or not re.fullmatch(r"[A-Za-z0-9_:]+", case["name"]):
                raise ValueError("Expected a complete Rust test name")
            if case["name"] in names:
                raise ValueError("Duplicate exact case in compiled variant")
            names.add(case["name"])
            if not isinstance(case["assertions"], str) or not case["assertions"].strip():
                raise ValueError("Every case must name its asserted behavior and controls")
    companions(value)
    return value, hashlib.sha256(raw).hexdigest()


def selection(plan):
    return [(variant["id"], case["name"], case["assertions"])
            for variant in plan["variants"] for case in variant["cases"]]


def product_variants(plan):
    return set(companions(plan))


def companions(plan):
    targets = {"product_root": ("product", "cli"), "dogfood_headless": ("product", "cli"),
               "integration": ("product",)}
    result = {}
    for variant in plan["variants"]:
        headless = (variant["package"], variant["kind"], variant["target"]) == (
            "grapt-product", "test", "dogfood_headless")
        if "companions" in variant and (not headless or variant["companions"] not in (
                ["product"], ["product", "cli"])):
            raise ValueError("Only headless consumers may explicitly select server-only companions")
        if (variant["package"] == "grapt-product" and variant["kind"] == "test"
                and variant["target"] in targets):
            result[variant["id"]] = tuple(variant.get("companions", targets[variant["target"]]))
    return result


def build_targets(path, ids, root):
    raw = path.read_bytes()
    if len(raw) > 65536:
        raise ValueError("Build target map exceeds its byte bound")
    value = json.loads(raw, object_pairs_hook=_object)
    if not isinstance(value, dict) or set(value) != set(ids):
        raise ValueError("Build target map must name every compiled variant exactly once")
    for destination in value.values():
        if (not isinstance(destination, str) or len(destination) > 1024 or
                not Path(destination).is_absolute() or
                not Path(destination).resolve().is_relative_to(root.resolve())):
            raise ValueError("Build targets must remain below the run-owned Cargo target root")
    return value


def command(variant, verb="test"):
    result = ["cargo", verb, "--locked", "-p", variant["package"]]
    result += ["--lib"] if variant["kind"] == "lib" else ["--" + variant["kind"], variant["target"]]
    if not variant["default_features"]:
        result.append("--no-default-features")
    if variant["features"]:
        result += ["--features", ",".join(variant["features"])]
    return result + ["--message-format=json"] + (["--no-run"] if verb == "test" else [])


def file_digest(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()
