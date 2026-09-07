#!/usr/bin/env python
"""Validate the GitHub Actions workflow files (syntactic + semantic).

Why this exists (recorded first-hosted-run repairs on ContractLens,
2026-08-21; carried over here before the first hosted run):

1. PyYAML accepts YAML that GitHub's workflow parser rejects (e.g. an
   unquoted `recorded: ~39 min` colon inside a step name — a parse error
   for GitHub that also produced bogus failed run records on every push).
2. Workflow SEMANTICS are invisible to any YAML parser: a step with both
   `run:` and a dangling `with:` block is valid YAML but invalid workflow
   structure ("Unexpected value 'with'"). That exact bug was shipped once;
   this script catches the class.

Run from the repository root:  python scripts/check-workflows.py

Exit 0 when every workflow parses and every step has exactly one of
`run`/`uses` and `with` only accompanies `uses`.
"""

import glob
import sys

import yaml

WORKFLOWS = sorted(glob.glob(".github/workflows/*.yml"))

errors = 0

for path in WORKFLOWS:
    try:
        doc = yaml.safe_load(open(path, encoding="utf-8"))
    except yaml.YAMLError as exc:
        print(f"{path}: YAML parse error: {exc}")
        errors += 1
        continue

    if not doc:
        print(f"{path}: empty document")
        errors += 1
        continue

    jobs = doc.get("jobs", {})
    if not isinstance(jobs, dict) or not jobs:
        print(f"{path}: no jobs found")
        errors += 1
        continue

    for job_name, job in jobs.items():
        for index, step in enumerate(job.get("steps", [])):
            label = f"job {job_name} step {index}"
            has_run = "run" in step
            has_uses = "uses" in step
            has_with = "with" in step
            if has_run and has_uses:
                print(f"{path}: {label}: both run and uses")
                errors += 1
            if has_with and not has_uses:
                print(f"{path}: {label}: with without uses")
                errors += 1
            if not has_run and not has_uses:
                print(f"{path}: {label}: neither run nor uses")
                errors += 1

if errors:
    print(f"{errors} workflow error(s)")
    sys.exit(1)

print(f"{len(WORKFLOWS)} workflow file(s) OK")
