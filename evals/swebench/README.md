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

Smoke (2026-10-22, 3 Verified instances): 3/3 non-empty patches,
5–20 min/instance on glm via bizbrain.
