---
name: review-a-run
description: Report what a finished media run produced and what it cost. Use after a run finishes, or when a person asks about a past generation.
---

# Review a run

1. Read the Task `media__run` started with `tasks/get`: its status and result. A failed
   run names why.
2. The result names `media://prediction/{prediction_id}/result` and the Artifact address
   of each output. `media://predictions` lists the person's runs, and
   `media://prediction/{prediction_id}` one run's state.
3. The Task's `ai.veoveo/usage` metadata holds the estimated and the actual cost.
4. Summarize the outputs, their media types and the final cost, and say when no actual
   cost was recorded yet.
