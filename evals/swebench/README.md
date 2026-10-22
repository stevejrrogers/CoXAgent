# SWE-bench harness for the CoXAgent stack

Runs the SAME engine the team runs on (`harxes` CLI) over SWE-bench
Verified/Lite instances and scores with the official harness.

    python3 -m venv .venv && ./.venv/bin/pip install datasets
    OPENAI_API_KEY=… ./.venv/bin/python run.py --limit 3 \
        --provider litellm --base-url <provider>
    # score (needs docker):
    pip install swebench && python -m swebench.harness.run_evaluation \
        --dataset_name princeton-nlp/SWE-bench_Verified \
        --predictions_path predictions.jsonl --run_id coxagent

On Apple Silicon the eval images are x86_64-only: pre-pull them with
`docker pull --platform linux/amd64 <image>` (zsh: quote `${id}:latest`)
and run the evaluation with `DOCKER_DEFAULT_PLATFORM=linux/amd64`.
Use the `SWE-bench/SWE-bench_Verified` dataset org (its rows carry the
`image` field swebench 5.x requires).

Smoke (2026-10-22, 3 Verified instances): **3/3 RESOLVED** by the
official harness — patches in 5–20 min/instance on glm via bizbrain.
