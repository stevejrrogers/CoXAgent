# Evals — the yardstick for agent quality

Fixed golden tasks scored deterministically, so a prompt or model change has a
number instead of a feeling. Run BEFORE and AFTER any prompt/model change:

    evals/run.sh            # claude (default)
    evals/run.sh opencode

Results append to `results/<date>-<engine>.json`; compare scores across dates.

| Task | Skill probed | Pass condition |
|------|--------------|----------------|
| fix-off-by-one | DEV: minimal correct fix, no test tampering | its own test compiles & passes, test untouched |
| review-planted-bug | SA: catches a planted iterate-while-mutating bug | verdict is `request_changes` |
| design-json | SA/BA: structured output discipline | JSON parses with all required keys |
| root-cause | debugging: names the mechanism, not the symptom | mentions port/address already in use |

Add a task: new dir under `tasks/` with `prompt.md` + `check.sh` (exit 0 =
pass) + optional `files/`. Keep checks DETERMINISTIC — no LLM judging.
