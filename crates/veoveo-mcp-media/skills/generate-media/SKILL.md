---
name: generate-media
description: Generate images, video, audio or 3D with a provider model, from choosing the model and reading its input schema to running it as a Task and finding its outputs in Artifact. Use whenever a task asks to create or edit media.
---

# Generate media

A run costs money: choose the model deliberately and check the input against its schema
before you start it.

## Choose a model

1. Call `media__models`, or read `media://models`, with `filters.query` for what you
   want and `filters.type` for the output (image, video, audio or 3D); continue with
   `nextCursor` as `cursor`, keeping the same filters and limit. Each model names its
   type, a description and, when the provider publishes one, its base price of one run.
2. For each candidate, call `media__model_schema` with its `model`, or read
   `media://model/{+model_id}`: the JSON Schema `run` checks the input against.
3. Prefer the model whose schema fits the task: an edit or image-to-image model to change
   an image, a video model for a brief. Tell the person the model and its price before an
   expensive run.

## Run it

1. Draft the `input` object to satisfy the schema exactly: required fields, enumerations,
   ranges and URL fields that must be reachable by the provider.
2. Call `media__run` with `model` and `input` as a Task (call it with Task support). Follow
   the Task's status until it finishes; a refused input fails at once with the schema's
   reason.
3. The finished Task returns the Artifact address of each output, and its
   `ai.veoveo/usage` metadata holds the estimated and actual cost.

## The outputs

Every output is an artifact owned by the person you work for, private to them until
shared, with the run in its provenance. Fetch it with `artifact__create_download` (skill
`skill://artifact/download-an-artifact/SKILL.md`) and share it with
`artifact__grant_artifact` (skill `skill://artifact/share-an-artifact/SKILL.md`).
