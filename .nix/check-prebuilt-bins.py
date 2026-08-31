#!/usr/bin/env python3
"""Fail if flake.nix, official provider binaries, and .nix/bins.json disagree.

The publish workflow packs whatever this file lists. A provider crate with a
binary that is missing from bins.json is how flake install broke after
opencode2: flake.nix grew a name that CI never packed.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BINS_PATH = ROOT / ".nix" / "bins.json"
FLAKE_PATH = ROOT / "flake.nix"
PROVIDERS = ROOT / "crates" / "providers"
GW_MAIN = ROOT / "crates" / "gw" / "src" / "main.rs"


def package_name(cargo_toml: Path) -> str | None:
    match = re.search(r'(?m)^name\s*=\s*"([^"]+)"', cargo_toml.read_text())
    return match.group(1) if match else None


def official_binaries() -> list[str]:
    names: list[str] = []
    if GW_MAIN.is_file():
        names.append("gw")
    for cargo in sorted(PROVIDERS.glob("*/Cargo.toml")):
        name = package_name(cargo)
        if name is None:
            sys.exit(f"no package name in {cargo}")
        has_bin = (cargo.parent / "src" / "main.rs").is_file() or "[[bin]]" in cargo.read_text()
        if has_bin:
            names.append(name)
    return names


def main() -> int:
    raw = BINS_PATH.read_text()
    try:
        bins = json.loads(raw)
    except json.JSONDecodeError as exc:
        print(f"{BINS_PATH}: {exc}", file=sys.stderr)
        return 1
    if not isinstance(bins, list) or not all(isinstance(x, str) and x for x in bins):
        print(f"{BINS_PATH} must be a JSON array of non-empty strings", file=sys.stderr)
        return 1
    if len(bins) != len(set(bins)):
        print(f"{BINS_PATH} has duplicate names", file=sys.stderr)
        return 1

    flake = FLAKE_PATH.read_text()
    if "builtins.fromJSON(builtins.readFile./.nix/bins.json)" not in re.sub(r"\s+", "", flake):
        print(
            "flake.nix must load bins from .nix/bins.json "
            "(builtins.fromJSON (builtins.readFile ./.nix/bins.json))",
            file=sys.stderr,
        )
        return 1

    expected = official_binaries()
    missing = [name for name in expected if name not in bins]
    extra = [name for name in bins if name not in expected]
    if missing or extra:
        print("prebuilt bins drifted from official binaries:", file=sys.stderr)
        if missing:
            print(f"  in crates but not {BINS_PATH.name}: {missing}", file=sys.stderr)
        if extra:
            print(f"  in {BINS_PATH.name} but no matching crate binary: {extra}", file=sys.stderr)
        return 1

    print(" ".join(bins))
    return 0


if __name__ == "__main__":
    sys.exit(main())
