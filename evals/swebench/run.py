#!/usr/bin/env python3
"""SWE-bench Verified runner for the CoXAgent stack.

Runs the SAME engine the team runs on (the `harxes` CLI against the
configured provider) over SWE-bench Verified instances, collects each run's
`git diff` as the prediction patch, and writes predictions.jsonl for the
official evaluation harness:

    python3 run.py --limit 3                 # smoke
    python3 run.py --limit 50 --split verified
    python -m swebench.harness.run_evaluation \
        --dataset_name princeton-nlp/SWE-bench_Verified \
        --predictions_path predictions.jsonl \
        --max_workers 4 --run_id coxagent

Design choices:
- The agent gets the issue text and a short contract (fix the bug, repo
  conventions, no test edits) — no gold hints, no test patch.
- Repos are cached under .cache/repos and reset hard per instance.
- Every run is bounded (--timeout, default 20 min) and the diff is taken
  even on timeout: a partial fix that passes is still a pass.
"""

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
CACHE = HERE / ".cache"
PROMPT = """You are fixing a real GitHub issue in this repository (already checked out at the buggy commit).

ISSUE:
{problem}

Rules:
- Find the root cause and fix it with the smallest correct change.
- Match the repo's existing style and conventions.
- Do NOT modify tests. Do NOT create new files unless strictly necessary.
- When done, make sure the working tree holds your final fix (just edit files; no commits needed).
"""


def sh(cmd, cwd=None, timeout=None, env=None):
    return subprocess.run(
        cmd, cwd=cwd, timeout=timeout, env=env, capture_output=True, text=True
    )


def load_instances(split, limit, offset):
    from datasets import load_dataset  # lazy: heavy import

    name = {
        "verified": "princeton-nlp/SWE-bench_Verified",
        "lite": "princeton-nlp/SWE-bench_Lite",
    }[split]
    ds = load_dataset(name, split="test")
    rows = list(ds)[offset : offset + limit]
    return name, rows


def checkout(inst):
    repo = inst["repo"]  # e.g. "django/django"
    base = inst["base_commit"]
    dest = CACHE / "repos" / repo.replace("/", "__")
    if not (dest / ".git").exists():
        dest.parent.mkdir(parents=True, exist_ok=True)
        r = sh(["git", "clone", f"https://github.com/{repo}.git", str(dest)], timeout=1800)
        if r.returncode != 0:
            raise RuntimeError(f"clone {repo}: {r.stderr[-300:]}")
    for cmd in (
        ["git", "fetch", "-q", "origin", base],
        ["git", "checkout", "-q", "-f", base],
        ["git", "clean", "-qfdx"],
    ):
        r = sh(cmd, cwd=dest, timeout=600)
        if r.returncode != 0:
            raise RuntimeError(f"{' '.join(cmd)}: {r.stderr[-300:]}")
    return dest


def run_agent(workdir, problem, args):
    env = dict(os.environ)
    prompt = PROMPT.format(problem=problem.strip()[:12000])
    cmd = [args.agent]
    if args.provider:
        cmd += ["--provider", args.provider]
    if args.model:
        cmd += ["--model", args.model]
    if args.base_url:
        cmd += ["--base-url", args.base_url]
    cmd.append(prompt)
    try:
        r = sh(cmd, cwd=workdir, timeout=args.timeout, env=env)
        tail = (r.stdout or r.stderr)[-400:]
    except subprocess.TimeoutExpired:
        tail = "(agent hit the wall-clock budget — taking the partial diff)"
    return tail


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--split", default="verified", choices=["verified", "lite"])
    ap.add_argument("--limit", type=int, default=3)
    ap.add_argument("--offset", type=int, default=0)
    ap.add_argument("--agent", default="harxes")
    ap.add_argument("--provider", default=os.environ.get("SWEB_PROVIDER", ""))
    ap.add_argument("--model", default=os.environ.get("SWEB_MODEL", ""))
    ap.add_argument("--base-url", default=os.environ.get("SWEB_BASE_URL", ""))
    ap.add_argument("--timeout", type=int, default=1200)
    ap.add_argument("--out", default=str(HERE / "predictions.jsonl"))
    args = ap.parse_args()

    dataset, rows = load_instances(args.split, args.limit, args.offset)
    print(f"{dataset}: running {len(rows)} instance(s)", flush=True)
    done = set()
    out_path = Path(args.out)
    if out_path.exists():
        for line in out_path.read_text().splitlines():
            try:
                done.add(json.loads(line)["instance_id"])
            except (ValueError, KeyError):
                pass
    with out_path.open("a") as out:
        for i, inst in enumerate(rows, 1):
            iid = inst["instance_id"]
            if iid in done:
                print(f"[{i}/{len(rows)}] {iid}: already predicted — skip", flush=True)
                continue
            t0 = time.time()
            try:
                wd = checkout(inst)
            except RuntimeError as e:
                print(f"[{i}/{len(rows)}] {iid}: SETUP FAIL {e}", flush=True)
                continue
            tail = run_agent(wd, inst["problem_statement"], args)
            diff = sh(["git", "diff"], cwd=wd, timeout=300).stdout
            mins = (time.time() - t0) / 60
            print(
                f"[{i}/{len(rows)}] {iid}: {len(diff)}B patch in {mins:.1f}m · {tail[:120]!r}",
                flush=True,
            )
            out.write(
                json.dumps(
                    {
                        "instance_id": iid,
                        "model_name_or_path": f"coxagent-{args.agent}"
                        + (f"-{args.model}" if args.model else ""),
                        "model_patch": diff,
                    }
                )
                + "\n"
            )
            out.flush()
    print(f"predictions → {out_path}", flush=True)


if __name__ == "__main__":
    sys.exit(main())
