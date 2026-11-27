"""Offline parsing of the gate's own JUnit reports.

Retained from the removed hosted_timing module with unchanged semantics:
skipped observations, including the partition-skipped cases that
report-skipped=all records in every shard artifact, never enter the
table; a later observation for the same case in one file wins and the
duplicate is counted; a malformed row is counted and discarded; a
missing time stays absent, never zero; an unparseable document rejects
the whole table. Durations are validated finite and nonnegative, and
every read is bounded by MAX_BYTES.
"""
import math
import xml.etree.ElementTree as ET

MAX_BYTES = 64 * 1024**2


def parse_junit(raw):
    """(binary, name) -> seconds for the executed cases of one junit file.

    Skipped observations, including the partition-skipped cases that
    report-skipped=all records in every shard artifact, never enter the
    table. A later observation for the same case wins and the duplicate
    is counted; a malformed row is counted and discarded; a missing time
    stays absent, never zero; an unparseable document rejects the whole
    table.
    """
    counters = {"observations": 0, "skipped": 0, "absent_time": 0,
                "malformed": 0, "duplicates": 0}
    try:
        document = ET.fromstring(raw)
    except ET.ParseError as error:
        raise ValueError("Archived junit does not parse: " + str(error))
    if document.tag not in ("testsuites", "testsuite"):
        raise ValueError("Archived junit has an unknown root element")
    table = {}
    for case in document.findall(".//testcase"):
        counters["observations"] += 1
        if case.find("skipped") is not None:
            counters["skipped"] += 1
            continue
        binary = case.get("classname") or ""
        name = case.get("name") or ""
        if (not binary or not name or "\r" in binary or "\n" in binary
                or "\r" in name or "\n" in name or len(binary) > 1024
                or len(name) > 1024):
            counters["malformed"] += 1
            continue
        elapsed = case.get("time")
        if elapsed is None:
            counters["absent_time"] += 1
            continue
        try:
            seconds = float(elapsed)
        except ValueError:
            counters["malformed"] += 1
            continue
        if not math.isfinite(seconds) or seconds < 0:
            counters["malformed"] += 1
            continue
        key = (binary, name)
        if key in table:
            counters["duplicates"] += 1
        table[key] = seconds
    return table, counters


def executed_identities(path):
    """The (binary, name) pairs a finished gate junit recorded as executed."""
    with open(path, "rb") as stream:
        raw = stream.read(MAX_BYTES + 1)
    if len(raw) > MAX_BYTES:
        raise ValueError("Gate junit exceeds its bound")
    table, _ = parse_junit(raw)
    return set(table)
