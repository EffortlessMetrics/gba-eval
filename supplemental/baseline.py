#!/usr/bin/env python3
"""Inspect pinned baseline inputs and compare retained stock/fork grading outputs.

Standard library only. Provision zstd separately for legacy-cache preflight.
This tool never repairs the corpus, changes scores, or invokes an LLM.
"""
from __future__ import annotations

import argparse
import hashlib
import io
import json
import math
import os
from pathlib import Path
import re
import struct
import subprocess
import sys
from typing import Any, BinaryIO

UPSTREAM = "92b585e5a0f6c7533bf69b9a4348b517a1b735e0"
FRAME_BYTES = 240 * 160 * 4
MAX_JSON = 32 * 1024 * 1024
HEX = re.compile(r"[0-9a-f]{64}\Z")
ID = re.compile(r"[a-zA-Z0-9_-]{1,128}\Z")


def require(ok: bool, message: str) -> None:
    if not ok:
        raise ValueError(message)


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def file_hash(path: Path) -> str:
    require(path.is_file() and not path.is_symlink(), f"not a regular file: {path}")
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def read_json(path: Path) -> Any:
    require(path.is_file() and not path.is_symlink(), f"not a regular JSON file: {path}")
    require(path.stat().st_size <= MAX_JSON, f"JSON exceeds limit: {path}")

    def pairs(items: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in items:
            require(key not in result, f"duplicate JSON key: {key}")
            result[key] = value
        return result

    def invalid(value: str) -> None:
        raise ValueError(f"nonfinite JSON number: {value}")

    with path.open("rb") as f:
        raw = f.read(MAX_JSON + 1)
    require(len(raw) <= MAX_JSON, "JSON grew past limit")
    result = json.loads(raw, object_pairs_hook=pairs, parse_constant=invalid)

    def finite(value: Any) -> None:
        if isinstance(value, float):
            require(math.isfinite(value), "nonfinite JSON number")
        elif isinstance(value, dict):
            for item in value.values():
                finite(item)
        elif isinstance(value, list):
            for item in value:
                finite(item)
    finite(result)
    return result


def new_json(path: Path, value: Any) -> None:
    data = (json.dumps(value, indent=2, sort_keys=True, allow_nan=False) + "\n").encode()
    with path.open("xb") as f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())


def git(root: Path, *args: str) -> bytes:
    return subprocess.run(["git", "-C", str(root), *args], check=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60).stdout


def tree(root: Path, ref: str) -> dict[str, tuple[str, str, str]]:
    result = {}
    for record in git(root, "ls-tree", "-rz", ref).split(b"\0"):
        if record:
            meta, name = record.split(b"\t", 1)
            mode, kind, oid = meta.decode().split()
            result[name.decode()] = (mode, kind, oid)
    return result


def safe_path(root: Path, relative: str) -> Path:
    p = Path(relative)
    require(not p.is_absolute() and ".." not in p.parts and relative, "unsafe relative path")
    target = root / p
    for ancestor in (target, *target.parents):
        if ancestor == root.parent:
            break
        require(not ancestor.is_symlink(), f"symlink in input path: {target}")
    return target


def source_snapshot(root: Path, base: str = UPSTREAM) -> dict[str, Any]:
    """Compare Git objects AND materialized working bytes, including LFS objects."""
    base_tree, head_tree = tree(root, base), tree(root, "HEAD")
    records = {}
    for name, entry in base_tree.items():
        mode, kind, oid = entry
        require(name in head_tree, f"baseline file removed: {name}")
        if kind == "commit":
            require(head_tree[name] == entry, f"reference submodule pin changed: {name}")
            records[name] = {"gitlink": oid}
            continue
        require(kind == "blob" and mode != "120000", f"unsupported baseline entry: {name}")
        original = git(root, "cat-file", "blob", oid)
        path = safe_path(root, name)
        require(path.is_file(), f"baseline input missing: {name}")
        actual_hash = file_hash(path)
        if name == "Cargo.toml":
            expected = original.replace(b'    "harness/dump",\n',
                                        b'    "harness/dump",\n    "harness/supplemental",\n')
            require(path.read_bytes() in (original, expected), "unexpected workspace/profile change")
        else:
            require(head_tree[name] == entry, f"baseline Git object changed: {name}")
            lfs = re.fullmatch(rb"version https://git-lfs.github.com/spec/v1\noid sha256:([0-9a-f]{64})\nsize ([0-9]+)\n?", original)
            if lfs:
                require(actual_hash == lfs[1].decode() and path.stat().st_size == int(lfs[2]),
                        f"LFS bytes missing or invalid: {name}")
            else:
                require(actual_hash == sha(original), f"dirty baseline input: {name}")
        records[name] = {"git_blob": oid, "sha256": actual_hash, "bytes": path.stat().st_size}
    return {"schema_version": 1, "baseline_commit": base,
            "head_commit": git(root, "rev-parse", "HEAD").decode().strip(),
            "original_inputs": records,
            "added_paths": sorted(set(head_tree) - set(base_tree)),
            "claim": "original source/input identity only; not score parity or original agent-condition equivalence"}


class CacheReader:
    def __init__(self, stream: BinaryIO):
        self.stream = stream
        self.consumed = 0

    def read(self, count: int) -> bytes:
        require(0 <= count <= 1 << 20, "oversized decoder read")
        out = bytearray()
        while len(out) < count:
            chunk = self.stream.read(count - len(out))
            require(bool(chunk), "truncated cache")
            out.extend(chunk)
        self.consumed += count
        require(self.consumed <= 16 * 1024**3, "decompressed cache exceeds limit")
        return bytes(out)

    def number(self, fmt: str) -> int | float:
        return struct.unpack("<" + fmt, self.read(struct.calcsize("<" + fmt)))[0]

    def skip(self, count: int) -> None:
        require(0 <= count <= 16 * 1024**3, "declared cache section exceeds limit")
        while count:
            n = min(count, 1 << 20)
            self.read(n)
            count -= n

    def digest(self) -> str:
        require(self.number("Q") == 64, "invalid cache digest length")
        value = self.read(64).decode("ascii")
        require(HEX.fullmatch(value) is not None, "invalid cache digest")
        return value


def decode_cache(stream: BinaryIO, *, reference: str, rom: str, replay: str, frames: int) -> dict[str, Any]:
    """Validate the pinned v7 bincode layout without unsafe byte-to-u32 casts."""
    require(type(frames) is int and 0 < frames <= 50_000, "invalid requested duration")
    c = CacheReader(stream)
    require(c.number("I") == 7, "unsupported cache version")
    require((c.digest(), c.digest(), c.digest()) == (reference, rom, replay), "cache input identity mismatch")
    recorded_frames, rate, boot = c.number("I"), c.number("I"), c.number("I")
    require(recorded_frames == frames, "cache duration differs from requested duration")
    require(0 < rate <= 1_048_576 and boot <= 600, "invalid cache rate/warmup")
    require(c.number("Q") == frames * FRAME_BYTES, "invalid frame byte count")
    c.skip(frames * FRAME_BYTES)
    require(c.number("Q") == frames, "invalid audio frame-count vector")
    samples = 0
    for _ in range(frames):
        n = c.number("I")
        require(n <= 262_144, "audio frame exceeds safety limit")
        samples += n
    require(c.number("Q") == samples * 2, "invalid interleaved audio length")
    c.skip(samples * 4)
    video_tau, audio_tau = c.number("f"), c.number("f")
    require(math.isfinite(video_tau) and 0 < video_tau <= 1, "invalid video threshold")
    require(math.isfinite(audio_tau) and audio_tau >= 0, "invalid audio threshold")
    require(not stream.read(1), "trailing cache data")
    return {"frames": frames, "sample_rate": rate, "boot_frames": boot,
            "audio_pairs": samples, "decoded_bytes": c.consumed,
            "video_threshold": video_tau, "audio_threshold": audio_tau}


def validate_cache(path: Path, **expected: Any) -> dict[str, Any]:
    require(path.is_file() and not path.is_symlink(), f"missing cache: {path}")
    with path.open("rb") as f:
        require(f.read(4) == b"\x28\xb5\x2f\xfd", f"cache is not materialized zstd: {path}")
    process = subprocess.Popen(["zstd", "-qdc", "--", str(path)], stdout=subprocess.PIPE,
                               stderr=subprocess.DEVNULL)
    try:
        assert process.stdout is not None
        result = decode_cache(process.stdout, **expected)
        require(process.wait(timeout=10) == 0, "zstd decode failed")
        return result
    finally:
        if process.poll() is None:
            process.kill()
        process.wait(timeout=10)
        if process.stdout:
            process.stdout.close()


def corpus_preflight(root: Path, check_caches: bool = True) -> dict[str, Any]:
    corpus = safe_path(root, "corpus")
    cases = read_json(corpus / "testcases.json")["testcases"]
    require(isinstance(cases, list) and 0 < len(cases) <= 1000, "invalid case manifest")
    reference = file_hash(safe_path(root, "reference/mesen.wasm"))
    roms = {}
    for path in sorted((corpus / "roms").rglob("*.gba")):
        path = safe_path(root, str(path.relative_to(root)))
        digest = file_hash(path)
        roms.setdefault(digest, []).append(str(path.relative_to(root)))
    ids = set()
    result = []
    for case in cases:
        name, digest, frames = case["id"], case["rom_sha256"], case["frames"]
        require(isinstance(name, str) and ID.fullmatch(name) is not None and name not in ids, "duplicate/unsafe case id")
        ids.add(name)
        require(isinstance(digest, str) and HEX.fullmatch(digest) is not None and digest in roms,
                f"required ROM unavailable: {name}")
        require(type(frames) is int and 0 < frames <= 50_000, f"invalid duration: {name}")
        replay = case.get("replay", "")
        replay_hash = file_hash(safe_path(corpus / "replays", replay)) if replay else sha(b"")
        record = {"id": name, "rom_sha256": digest, "rom_paths": roms[digest],
                  "replay_sha256": replay_hash, "frames": frames}
        if check_caches:
            path = safe_path(corpus / "reference-cache", name + ".refcache")
            record["cache"] = validate_cache(path, reference=reference, rom=digest, replay=replay_hash, frames=frames)
            record["cache_sha256"] = file_hash(path)
        result.append(record)
    return {"schema_version": 1, "reference_sha256": reference, "case_count": len(cases),
            "manifest_sha256": file_hash(corpus / "testcases.json"), "cases": result,
            "cache_validation": "pinned_v7" if check_caches else "not_checked"}


def ulps(a: float, b: float) -> int:
    require(math.isfinite(a) and math.isfinite(b) and a >= 0 and b >= 0, "unsupported aggregate number")
    return abs(struct.unpack("<Q", struct.pack("<d", a))[0] - struct.unpack("<Q", struct.pack("<d", b))[0])


def aggregate_path(path: tuple[str, ...]) -> bool:
    return path == ("overall",) or (len(path) >= 3 and path[0] == "sections" and
                                   (path[-1] == "score" or (len(path) == 4 and path[2] == "subsystems")))


def compare_values(stock: list[Any], candidate: Any, path: tuple[str, ...] = (), diagnostics: list | None = None) -> list:
    """Only demonstrably noisy aggregate fields get a small calibrated ULP allowance."""
    if diagnostics is None:
        diagnostics = []
    first = stock[0]
    if path == ("graded_at",):
        require(all(isinstance(v, str) for v in [*stock, candidate]), "invalid timestamp metadata")
        return diagnostics
    require(all(type(v) is type(first) for v in [*stock, candidate]), f"type mismatch at {path}")
    if isinstance(first, dict):
        require(all(set(v) == set(first) for v in [*stock, candidate]), f"key membership changed at {path}")
        for key in sorted(first):
            compare_values([v[key] for v in stock], candidate[key], (*path, key), diagnostics)
    elif isinstance(first, list):
        require(all(len(v) == len(first) for v in [*stock, candidate]), f"array length changed at {path}")
        for i in range(len(first)):
            compare_values([v[i] for v in stock], candidate[i], (*path, str(i)), diagnostics)
    elif isinstance(first, float) and aggregate_path(path):
        require(all(math.isfinite(v) and 0 <= v <= 1 for v in [*stock, candidate]), "invalid aggregate score")
        spread = max(ulps(first, v) for v in stock)
        require(spread <= 16, f"stock aggregate instability exceeds maximum at {path}")
        allowance = min(16, spread + 2) if spread else 0
        distance = min(ulps(v, candidate) for v in stock)
        require(distance <= allowance, f"score changed at {path}: stock={stock}, candidate={candidate}")
        diagnostics.append({"path": list(path), "stock_spread_ulps": spread,
                            "allowed_ulps": allowance, "nearest_distance_ulps": distance})
    else:
        require(all(v == first for v in stock) and candidate == first, f"value changed at {path}")
    return diagnostics


def compare_runs(stock: list[Path], candidate: Path, expected_ids: set[str] | None = None) -> dict[str, Any]:
    require(len(stock) >= 2, "at least two stock repetitions are required")
    directories = [*stock, candidate]
    summaries = [read_json(p / "summary.json") for p in directories]
    for s in summaries:
        require(isinstance(s.get("per_testcase"), dict) and bool(s["per_testcase"]), "empty/missing graded cases")
        if expected_ids is not None:
            require(set(s["per_testcase"]) == expected_ids, "graded membership differs from manifest")
    calibration = compare_values(summaries[:-1], summaries[-1])
    receipt = {"summary_hashes": [file_hash(p / "summary.json") for p in directories],
               "aggregate_calibration": calibration, "cases": {}}
    for name in sorted(summaries[0]["per_testcase"]):
        require(ID.fullmatch(name) is not None, "unsafe testcase id")
        paths = [p / (name + ".json") for p in directories]
        if any(s["per_testcase"][name].get("skipped", False) for s in summaries):
            receipt["cases"][name] = {"disposition": "same recorded candidate failure; no per-frame claim"}
            continue
        values = [read_json(p) for p in paths]
        require(all(v == values[0] for v in values), f"per-case observations changed: {name}")
        receipt["cases"][name] = {"sha256": [file_hash(p) for p in paths], "disposition": "exact semantic equality"}
    return {"schema_version": 1, "claim": "score parity for the recorded artifacts/environment only",
            "result": "pass", **receipt}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("snapshot"); p.add_argument("root", type=Path); p.add_argument("output", type=Path)
    p = sub.add_parser("preflight"); p.add_argument("root", type=Path); p.add_argument("output", type=Path)
    p = sub.add_parser("compare"); p.add_argument("candidate", type=Path); p.add_argument("output", type=Path)
    p.add_argument("--stock", type=Path, action="append", required=True); p.add_argument("--manifest", type=Path, required=True)
    args = parser.parse_args()
    if args.command == "snapshot":
        result = source_snapshot(args.root.resolve())
    elif args.command == "preflight":
        result = corpus_preflight(args.root.resolve())
    else:
        manifest = read_json(args.manifest)
        expected = {case["id"] for case in manifest["testcases"]}
        result = compare_runs(args.stock, args.candidate, expected)
    new_json(args.output, result)
    print(f"{args.command}: recorded {args.output}")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(f"baseline verification failed: {error}", file=sys.stderr)
        sys.exit(2)
