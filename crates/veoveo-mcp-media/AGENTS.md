# Media MCP Server — Agent Manual

Delta over the repository root `AGENTS.md`. [`DESIGN.md`](DESIGN.md) is the design and
operational contract.

## Purpose

Image, video and audio generation with provider models. A run is a durable Task that
hands the generation to the provider, parks until the provider's signed webhook
settles it, stores each output in Artifact for the run's caller and records the run's
cost in core's usage ledger.

## Invariants

- Owns the `media://` URI scheme and the PostgreSQL schema `media`, and its SQL touches
  no other schema. A run's owner, status, failure and times are its Task's, read
  through core's Task runtime; `media.prediction` keeps only what Media alone knows
  about the run, and nothing copies the Task's state. Identity: slug
  `media`, MCP `/media/mcp`, provider callbacks `/media/operations/{operation_id}/{token}`,
  port 8802.
- The provider stays inside `src/provider/`. User-facing names, configuration keys and
  documents name no provider; the API key and webhook secret are
  `MEDIA_PROVIDER_API_KEY` and `MEDIA_PROVIDER_WEBHOOK_SECRET`.
- The provider address is required configuration with no default. Tests and the local
  installation run the fake provider from the `testing` feature. Only
  `tests/provider_qualification.rs` reaches the real provider, ignored by default and
  gated by `MEDIA_PROVIDER_QUALIFICATION=1`; it is never part of the gate.
- The installation chooses a run's completion: its public origin, `VEOVEO_PUBLIC_ORIGIN`,
  selects the provider's webhooks to `{origin}/media/operations/{operation_id}/{token}`,
  and its absence bounded checks of the prediction's status endpoint. Never fall back from one to the other, and never call a status endpoint
  off the provider's origin.
- A run reaches the provider only through `ctx.operation(..).dispatch`, which records
  the operation before the call. A dispatch that may have reached the provider waits
  for its webhook and never submits again. Nothing checks for cancellation between the
  dispatch and `ctx.wait`, so a cancellation reaches the provider through the wait.
- Every provider call carries a timeout, and no error names a URL: a submission's URL
  carries the callback's bearer token.
- The webhook parser checks the HMAC signature and the signing time before it reads the
  body. The runtime stores each event once, and the first final event wins.
- Each output is one durable step that streams the provider's bytes into
  `ArtifactClient` from the Task's binding, with the idempotency key
  `media:{task}:output:{index}` and a size bound. No output is held in memory whole,
  and outputs come only from allowed hosts.
- Usage records go through `ctx.usage()`: the estimate before dispatch, the actual cost
  after the provider's bill is reconciled through bounded checks.
- A run's owner, tenant and labels come from the forwarded identity or the Task's
  owner. A tool argument never names them. Prediction reads filter by owner and labels
  inside the statement, and a run the reader may not see reads as missing.
- Every statement is a `.sql` file under `src/store/` run through sqlx's checked macros.
  The tests compile Artifact's queries too, so regenerate `.sqlx` against a database
  with core's, Artifact's and Media's migrations, from an empty target directory.
- Row triggers notify the `media` channel with each run's Task and tenant. Do not
  notify subscribers from handlers.

## Build And Test

- `cargo test -p veoveo-mcp-media --all-features` with the PostgreSQL 18 server
  programs and `weed` (SeaweedFS) on `PATH`, as the `dev` image provides them.
- `cargo test -p veoveo-mcp-media --no-default-features --features contract` checks the
  contract alone.
- `tests/run.rs` runs generations against the fake provider through the SDK's mesh with
  Artifact in process, in both completion modes; it needs the `testing` feature.
- `tests/provider_qualification.rs` bills the real provider once when run with
  `MEDIA_PROVIDER_QUALIFICATION=1` and `--run-ignored only`; the root README says how.
- `tests/database_conformance.rs` runs core's database checks over every statement and
  the `media` schema; regenerate `.sqlx` before running it.
- `docker build --target check .` runs the gate.

## Contract Compliance

<!-- veoveo:contract-compliance:start -->
Contract: https://veoveo.ai/contracts/hosted-mcp

- C01: met
- C02: met
- C03: met — run is a task-augmented tool on the shared Task runtime; it parks on provider operations and resumes from durable steps
- C04: met — runs page by key at 100 per page, filtered by owner and labels inside each statement; the model catalog pages an immutable registry snapshot with cursors bound to its digest
- C05: met
- C06: met
- C07: met
- C08: met
- C09: met
- C10: met
- C11: met — outputs reach Artifact through the Gateway with the run's Task delegation; Media holds no Artifact credential
- C12: met
- C13: met
- C14: met — outputs stream from the provider into Artifact through signed object-storage URLs; the public callback route takes only signed provider events and serves no bytes
- C15: pending — the OCI image builds; the Helm chart is pending
- C16: met
- C17: met
- C18: met
- C19: met
- C20: met
- C21: met
- C22: met
- C23: met
- C24: met
- C25: met
- C26: met
- C27: met
- C28: met
- C29: met — every replica runs the Task worker, the catalog refresh and the sweep; Task claims and FOR UPDATE SKIP LOCKED hand each Task and row to one replica
- C30: met
- C31: pending — installed Discover and list readiness qualification is pending
- C33: pending — Owner naming producers, consumers and installed qualification are pending.
<!-- veoveo:contract-compliance:end -->
