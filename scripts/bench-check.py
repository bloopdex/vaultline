"""The benchmark regression check (ADR-007): run the release benchmark
example and compare each metric against docs/benchmarks/baseline.json.
A metric above 3x its baseline fails the check — the threshold is
generous for a first baseline (same-machine noise dominates, not
progress). Exit 0 all-ok, 1 regression, 2 tooling failure.

Usage: python scripts/bench-check.py
"""

import json
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
BASELINE = REPO / "docs" / "benchmarks" / "baseline.json"
THRESHOLD = 3.0


def main() -> int:
    if not BASELINE.exists():
        print(f"missing baseline: {BASELINE}")
        return 2

    baseline = json.loads(BASELINE.read_text(encoding="utf-8"))
    baseline.pop("note", None)

    run = subprocess.run(
        ["cargo", "run", "--release", "--example", "bench"],
        cwd=REPO,
        capture_output=True,
        text=True,
    )
    if run.returncode != 0:
        print("benchmark run failed:")
        print(run.stderr[-2000:])
        return 2

    report = json.loads(run.stdout)
    failures = []
    for name, expected in baseline.items():
        measured = report[name]["median_ms"]
        ratio = measured / expected
        status = "OK" if ratio < THRESHOLD else "REGRESSION"
        print(f"{status:>10}  {name}: {measured:.4f} ms (baseline {expected:.4f} ms, {ratio:.1f}x)")
        if ratio >= THRESHOLD:
            failures.append(name)
    if failures:
        print(f"REGRESSION: {', '.join(failures)} above {THRESHOLD}x baseline")
        return 1
    print("benchmarks within thresholds")
    return 0


if __name__ == "__main__":
    sys.exit(main())
