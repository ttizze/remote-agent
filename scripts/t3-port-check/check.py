#!/usr/bin/env python3
"""Score fixed T3 behavioral cases; omitted cases and failed guards cannot win."""
import json
from fnmatch import fnmatch
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
CASES = json.loads(Path(__file__).with_name("cases.json").read_text())
HOLDOUT = sys.argv[1:] == ["--holdout"]
if sys.argv[1:] and not HOLDOUT:
    sys.exit("expected no arguments or --holdout")
SCORED = set() if HOLDOUT else {(crate, name) for crate, names in CASES["scenarios"].items() for name in names}
GUARDS = {(crate, name) for crate, names in CASES["holdout" if HOLDOUT else "guards"].items() for name in names}
ALL = SCORED | GUARDS
expression = " | ".join(f"test(={name})" for name in sorted({name for _, name in ALL}))
command = [str(ROOT / "scripts/dev-env.sh"), "cargo", "nextest", "run", "--locked",
           "--no-fail-fast", "--retries", "0", "--color", "never"]
for crate in sorted({crate for crate, _ in ALL}):
    command += ["-p", crate]
command += ["-E", expression]
result = subprocess.run(command, cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
log = ROOT / (".hill-climb/t3-port/holdout.txt" if HOLDOUT else ".hill-climb/t3-port/tests.txt")
log.parent.mkdir(parents=True, exist_ok=True)
log.write_text(result.stdout)
passed = {(binary.split("::", 1)[0], name) for binary, name in re.findall(r"PASS\s+\[[^\]]+\]\s+\([^\n]+?\)\s+(\S+)\s+(\S+)", result.stdout)}
if "Summary" not in result.stdout or not GUARDS.issubset(passed):
    print(result.stdout[-12000:])
    print("Invalid check: build, builder, or fixed guard failed", file=sys.stderr)
    for crate, name in sorted(GUARDS - passed):
        print(f"guard did not pass: {crate} {name}", file=sys.stderr)
    sys.exit(1)
missing = SCORED - passed
remaining = [] if HOLDOUT else [path for path in CASES["obsolete_files"] if (ROOT / path).exists()]
obsolete_symbols = [] if HOLDOUT else [
    rule['name']
    for rule in CASES["obsolete_symbols"]
    if any(re.search(rule["pattern"], path.read_text())
           for glob in rule["globs"] for path in ROOT.glob(glob) if path.is_file()
           and str(path.relative_to(ROOT)) not in rule["exclude_paths"]
           and not any(fnmatch(str(path.relative_to(ROOT)), excluded) for excluded in rule["exclude"]))
]
for crate, name in sorted(missing):
    print(f"scenario did not pass: {crate} {name}")
for path in remaining:
    print(f"obsolete implementation remains: {path}")
for symbol in obsolete_symbols:
    print(f"obsolete contract remains: {symbol}")
print(f"Passed {len(SCORED & passed)}/{len(SCORED)} scenarios; {len(GUARDS)} guards")
print(len(missing) + len(remaining) + len(obsolete_symbols))
