#!/usr/bin/env python3
"""Check canonical behavior; a missing test or a regressed guard cannot win."""
import json
from fnmatch import fnmatch
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
CASE_PATH = Path(__file__).with_name("cases.json")
CASES = json.loads(CASE_PATH.read_text())
MODE = sys.argv[1:]
if MODE not in ([], ["--holdout"], ["--baseline"], ["--milestone"]):
    sys.exit("expected --baseline, --holdout, --milestone, or no arguments")
HOLDOUT = MODE == ["--holdout"]
BASELINE = MODE == ["--baseline"]
MILESTONE = MODE == ["--milestone"]


def entries(groups):
    return {(binary, name) for binary, names in groups.items() for name in names}


def grouped(cases):
    result = {}
    for binary, name in sorted(cases):
        result.setdefault(binary, []).append(name)
    return result


held_out = entries(CASES["holdout"])
scored = set() if HOLDOUT or BASELINE else entries(CASES["scenarios"])
all_guards = entries(CASES["guards"])
# These previously passing checks exercise unchanged credentials/audio/browser
# resources. The full common suite retains them; avoid their real-time budgets
# on every conversation-storage step. A failing scenario cannot be deferred.
deferred = entries(CASES.get("milestone_guards", {}))
if not deferred <= all_guards or deferred & entries(CASES["scenarios"]):
    sys.exit("Invalid check: only an already passing guard may be deferred")
guards = held_out if HOLDOUT else all_guards if MILESTONE else all_guards - deferred
crates = sorted({binary.split("::", 1)[0] for binary, _ in held_out | scored | guards} | (set() if HOLDOUT else {"agent-protocol", "agent-core", "host-daemon"}))
command = [str(ROOT / "scripts/dev-env.sh"), "cargo", "nextest"]
arguments = ["--locked", "--color", "never"]
for crate in crates:
    arguments += ["-p", crate]
excluded = held_out | (set() if HOLDOUT or MILESTONE else deferred)
held_out_filter = " | ".join(f"test(={name})" for _, name in sorted(excluded))
if held_out_filter:
    arguments += ["-E", held_out_filter if HOLDOUT else f"not({held_out_filter})"]

if BASELINE:
    listing = subprocess.run(command + ["list", *arguments, "--message-format", "json"], cwd=ROOT, text=True, capture_output=True)
    if listing.returncode:
        sys.exit(listing.stderr[-12000:])
    suites = json.loads(listing.stdout)["rust-suites"]
    expected = {(binary, name) for binary, suite in suites.items() for name, case in suite["testcases"].items() if not case["ignored"] and case["filter-match"]["status"] == "matches"}
else:
    expected = scored | guards

result = subprocess.run(command + ["run", *arguments, "--no-fail-fast", "--retries", "0"], cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
log = ROOT / (".hill-climb/t3-port/holdout.txt" if HOLDOUT else ".hill-climb/t3-port/setup-tests.txt" if BASELINE else ".hill-climb/t3-port/tests.txt")
log.parent.mkdir(parents=True, exist_ok=True)
log.write_text(result.stdout)
completed = re.findall(r"\b(PASS|FAIL|TIMEOUT|LEAK|ABORT)\s+\[[^\]]+\]\s+\([^\n]+?\)\s+(\S+)\s+(\S+)", result.stdout)
passed = {(binary, name) for status, binary, name in completed if status == "PASS"}
failed = {(binary, name) for status, binary, name in completed if status != "PASS"}
seen = passed | failed
if result.returncode not in (0, 100) or "Summary" not in result.stdout or not expected or seen != expected or passed & failed:
    print(result.stdout[-12000:])
    for binary, name in sorted(expected - seen)[:10]:
        print(f"test did not run: {binary} {name}", file=sys.stderr)
    for binary, name in sorted(seen - expected)[:10]:
        print(f"new test needs baseline: {binary} {name}", file=sys.stderr)
    sys.exit("Invalid check: build, runner, or complete test inventory failed")

if BASELINE and not guards.issubset(passed):
    for binary, name in sorted(guards - passed):
        print(f"existing guard regressed: {binary} {name}", file=sys.stderr)
    sys.exit("Refusing baseline: repair or independently justify the changed guard contract")

if BASELINE:
    CASES["scenarios"] = grouped(failed)
    CASES["guards"] = grouped(passed | deferred)
    CASES["obsolete_files"] = [path for path in CASES["obsolete_files"] if path not in {
        "crates/agent-protocol/src/conversation/records/view.rs",
        "crates/host-daemon/src/host_rpc/storage/activity.rs",
        "crates/host-daemon/src/host_rpc/storage/titles.rs",
    }]
    for rule in CASES["obsolete_symbols"]:
        rule["pattern"] = rule["pattern"].replace("\\\\", "\\")
    CASES["obsolete_symbols"] += [rule for rule in [
        {"name": "native-detail-query", "globs": ["crates/host-daemon/src/host_rpc/runtime/bodies.rs"], "exclude": [], "exclude_paths": [], "pattern": r"\bfn\s+read_canonical_item\b"},
        {"name": "native-catalog-search", "globs": ["crates/host-daemon/src/host_rpc/service/titles.rs"], "exclude": [], "exclude_paths": [], "pattern": r"\bsession_pages\s*\("},
    ] if rule["name"] not in {existing["name"] for existing in CASES["obsolete_symbols"]}]
    CASE_PATH.write_text(json.dumps(CASES, ensure_ascii=False, indent=2) + "\n")
    scored, guards = failed, passed
    print(f"Baseline inventory: {len(scored)} failing cases; {len(guards)} passing guards")

if not guards.issubset(passed):
    print(result.stdout[-12000:])
    for binary, name in sorted(guards - passed):
        print(f"guard did not pass: {binary} {name}", file=sys.stderr)
    sys.exit("Invalid check: fixed guard failed")
remaining = [] if HOLDOUT else [path for path in CASES["obsolete_files"] if (ROOT / path).exists()]
obsolete = [] if HOLDOUT else [
    rule["name"] for rule in CASES["obsolete_symbols"]
    if any(re.search(rule["pattern"], path.read_text())
           for glob in rule["globs"] for path in ROOT.glob(glob) if path.is_file()
           and str(path.relative_to(ROOT)) not in rule["exclude_paths"]
           and not any(fnmatch(str(path.relative_to(ROOT)), exclusion) for exclusion in rule["exclude"]))
]
for binary, name in sorted(scored - passed):
    print(f"scenario did not pass: {binary} {name}")
for path in remaining:
    print(f"obsolete implementation remains: {path}")
for name in obsolete:
    print(f"obsolete implementation remains: {name}")
print(f"Passed {len(scored & passed)}/{len(scored)} scenarios; {len(guards)} guards")
print(len(scored - passed) + len(remaining) + len(obsolete))
