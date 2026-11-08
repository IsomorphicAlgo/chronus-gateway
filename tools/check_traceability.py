#!/usr/bin/env python3
"""Checks docs/TRACEABILITY.md against the code (Tranche 5.4, Methodology D-057).

Fails (exit 1) when
  * a referenced test `path.rs::name` is missing (no `fn name(` in that file),
  * a referenced file path does not exist,
  * an alarm constant `pub const FLAG_*` in validate.rs is not named in the register,
  * a tolerance ID `T-XXX` used under crates/ is not defined as a register row.

Standard library only; run from anywhere: `python3 tools/check_traceability.py`.
"""

from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
DOC = ROOT / "docs" / "TRACEABILITY.md"
VALIDATE = ROOT / "crates" / "gateway" / "src" / "validate.rs"
# Aliases kept for compatibility; they share a row with the constant they alias.
FLAG_ALIASES = {"FLAG_RSSI_RESERVED"}


def main() -> int:
    doc = DOC.read_text(encoding="utf-8")
    errors: list[str] = []

    tests = set(re.findall(r"`([\w./-]+\.rs)::(\w+)`", doc))
    for path, name in sorted(tests):
        f = ROOT / path
        if not f.is_file():
            errors.append(f"{path}::{name}: file not found")
        elif not re.search(rf"\bfn {re.escape(name)}\s*[(<]", f.read_text(encoding="utf-8")):
            errors.append(f"{path}::{name}: no such function")

    paths = set(re.findall(r"`((?:crates|fuzz|tools|docs|demo|deploy|\.github)/[\w./-]+)`", doc))
    for p in sorted(paths):
        if "::" not in p and not (ROOT / p).exists():
            errors.append(f"{p}: path not found")

    flags = re.findall(r"pub const (FLAG_\w+)", VALIDATE.read_text(encoding="utf-8"))
    for flag in flags:
        if flag not in FLAG_ALIASES and f"`{flag}`" not in doc:
            errors.append(f"{flag}: alarm constant has no row in the tolerance register")

    defined = set(re.findall(r"^\| \*\*(T-[A-Z]+)\*\* \|", doc, flags=re.M))
    used: dict[str, str] = {}
    for f in sorted((ROOT / "crates").rglob("*.rs")):
        for m in re.finditer(r"\bT-([A-Z]{2,})\b", f.read_text(encoding="utf-8")):
            used.setdefault("T-" + m.group(1), str(f.relative_to(ROOT)))
    for tid, where in sorted(used.items()):
        if tid not in defined:
            errors.append(f"{tid} (used in {where}): not defined in the tolerance register")

    for e in errors:
        print(f"traceability: {e}", file=sys.stderr)
    print(f"traceability: {len(tests)} test references, {len(paths)} paths, {len(flags)} alarm "
          f"constants, {len(defined)} tolerance IDs ({len(used)} used in code): "
          + ("FAIL" if errors else "ok"))
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
