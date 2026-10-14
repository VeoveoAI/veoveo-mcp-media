# Media MCP Design

This document is the design and operational contract for the `veoveo-mcp-media` crate.

Media generates images, video and audio with a provider's models. A run is a durable
Task: Media hands the generation to the provider, parks the Task until the provider
reports the outcome, streams each output into Artifact for the run's caller and records
what the run cost. Callers see provider-neutral models, predictions and results; the
provider's identity stays inside the server.

## Service Identity

```text
crate        veoveo-mcp-media
binary       veoveo-mcp-media
repository   veoveo-mcp-media
slug         media
URI scheme   media
permissions  media.*; uses artifact.artifacts.read and artifact.artifacts.write
MCP          /media/mcp
callbacks    /media/operations/{operation_id}/{token}
liveness     /livez
readiness    /readyz
port         8802
```

## Standards And Protocols

| Standard or protocol | Implemented profile |
|---|---|
| [Model Context Protocol](https://modelcontextprotocol.io/specification/) | Version `2026-07-28`, JSON-RPC 2.0 over Streamable HTTP, under the hosted MCP contract `https://veoveo.ai/contracts/hosted-mcp` that `veoveo-sdk-rust` 67314b4 implements. The server exposes tools, `run` as a Task through the Tasks extension, resources and templates, prompts, completions of model identities, `subscriptions/listen` and typed structured content. |
| [JSON Schema Draft 2020-12](https://json-schema.org/draft/2020-12/) | Tool inputs and results and resource contents. A run's input is checked with jsonschema 0.58.6 against the JSON Schema the provider publishes for the model; that schema's shape is the provider's. |
| [URI Template RFC 6570](https://www.rfc-editor.org/rfc/rfc6570) | Discovery declares simple variables, one `{+model_id}` reserved expansion for a model's `/`-separated components, and collection pages that continue through a `cursor` query parameter. |
| [JWT access tokens RFC 9068](https://www.rfc-editor.org/rfc/rfc9068.html) and [Token Exchange RFC 8693](https://www.rfc-editor.org/rfc/rfc8693.html) | The Gateway forwards each request with an access token naming the caller. A run calls Artifact through the Gateway with a token the SDK obtains by exchange for the run's Task, narrowed to Media's declared uses of Artifact that the caller holds. |
| [HMAC-SHA256, RFC 2104](https://www.rfc-editor.org/rfc/rfc2104) | The provider signs each webhook over `{webhook-id}.{webhook-timestamp}.{body}` with the account's webhook secret; the `webhook-signature` header carries `v3,<hex>` signatures. Media refuses a signing time more than 300 seconds from its clock. |
| [FOCUS 1.4](https://focus.finops.org/focus-specification/) usage records in core's [usage ledger](https://github.com/VeoveoAI/veoveo-core/blob/d5353ac1c4bba0cea6e115c734a000e598055aa6/platform/usage/DESIGN.md) | One usage kind, `media.generation`, in the FOCUS service category "AI and Machine Learning" with the unit `Requests`, and amounts in [ISO 4217](https://www.iso.org/iso-4217-currency-codes.html) US dollars. |
| [Veoveo server registration](https://github.com/VeoveoAI/veoveo-sdk-rust) | Media registers through RFC 7591, removes its registration through RFC 7592, and declares its permissions and its uses of Artifact in `ai.veoveo/authorization`. |
| [PostgreSQL 18](https://www.postgresql.org/docs/18/) | Server 18.6 through sqlx 0.9.0, under the [installation database design](https://github.com/VeoveoAI/veoveo-core/blob/d5353ac1c4bba0cea6e115c734a000e598055aa6/docs/DATABASE.md) of `veoveo-core`. Media owns the schema `media`, applies its own migrations and runs every statement from a `.sql` file that sqlx checks. Row triggers publish changes on the `media` channel. |
| The provider's HTTP API | The subset Media calls: the model registry, a submission with an optional webhook, a prediction's status endpoint, a billing search and a prediction's deletion. Every response is wrapped in `{code, message, data}`. |

## Domain Contract

Media answers these questions for a caller:

- Which models can generate what, at what list price, and what input does each take?
- What did my runs produce, where are the outputs, and what did each run cost?

A run's input belongs to the model and passes through to the provider. Everything else
Media serves is typed: model and prediction identities, catalog pages, predictions and
generation results.

## Architecture

### Library Features

| Feature | Holds | Depends on |
|---|---|---|
| `contract` | The identities, resource addresses, tool requests and results, `MediaPermission`, and `MediaProvider` with a `ToolCall` for each tool | `veoveo-types`, the SDK's `contract` feature, Serde and Schemars |
| `server` (default) | The hosted server, the run worker, the provider client, the catalog and the `media` schema | The SDK's `server` feature, core's Task runtime and store, Artifact's `client` feature, sqlx, reqwest and jsonschema |
| `testing` | The fake provider and its binary, `veoveo-mcp-media-fake-provider` | `server` |

### Hosted Process

`veoveo-mcp-media` reads the configuration, registers with the Gateway, connects with
the registration's database login, applies its migrations, declares its usage kind and
serves MCP and the callback route. Beside them run the run worker on the SDK's
`TaskWorker`, the catalog refresh, the change watcher and the sweep. SIGTERM stops
claiming runs, lets running ones finish for up to 30 seconds and leaves the rest for
another instance's recovery, then deletes the registration.

## MCP Surface

### Tools

| Tool | Does | Requires |
|---|---|---|
| `models` | A page of the catalog, filtered by `query` or `type` | `media.models.read` |
| `model_schema` | One model with its input schema | `media.models.read` |
| `run` | Runs a model as a Task; a plain call is refused | `media.generations.run` (billed) |

`run` checks the model and the input against the catalog before it creates the Task, so
a bad request fails at once and costs nothing.

### Resources

| Address | Holds |
|---|---|
| `media://docs`, `media://docs/{doc_id}`, `media://contract` | The embedded documents and the contract declaration |
| `media://models{?query,type,limit,cursor}` | A catalog page of 20 models by default and at most 100, in model identity order |
| `media://model/{+model_id}` | One model and the JSON Schema of its input |
| `media://predictions{?cursor}` | A page of the caller's runs, newest first, 100 at a time |
| `media://prediction/{prediction_id}` | One of the caller's runs by the provider's prediction |
| `media://prediction/{prediction_id}/result` | A succeeded run's result |

A run is visible to its owner while the owner holds every data label the run carries.
A run the reader may not see reads as missing. The prediction listing and single
predictions accept subscriptions; the catalog, documents and results do not change.

### Prompts

`media-model-select`, `media-image-edit` and `media-video-generate` draft a run from a
goal, and `media-task-review` reviews a finished run's outputs and cost.

## Runs

A run's Task follows these steps, and a restarted Task resumes at the first one that
has not finished:

1. Record the run for the caller's listing.
2. Before the run reaches the provider: check the input against the model's schema,
   stop if the Task was cancelled, and record the estimated cost, the model's list
   price, as a reservation in the usage ledger.
3. Dispatch the generation as a core operation. The runtime records the operation
   before the provider is called. A submission whose outcome is unknown, or a restarted
   dispatch that may have reached the provider, waits for the provider's report and
   never submits again. A submission the provider refuses fails the run.
4. Wait. The Task parks without a worker until the outcome arrives or the deadline
   passes. Nothing checks for cancellation between the dispatch and the wait, so a
   cancellation always reaches the provider through the wait.
5. On success, store each output as one durable step (below), reconcile the bill and
   complete with the result. On a provider failure, reconcile the bill and fail with
   the provider's reason. At the deadline, the run ends unresolved.

### Completion

The installation chooses how runs learn their outcome, and Media never falls back from
one to the other:

| Configuration | Completion |
|---|---|
| `VEOVEO_PUBLIC_ORIGIN` set | Events. The installation's public origin holds every server under its own path, and the submission names `{origin}/media/operations/{operation_id}/{token}` as the webhook, such as `https://veoveo.example.com/media/operations/…`. The callback route checks the signature and the signing time, the runtime matches the token's digest and stores each event once, and the first final event wins. |
| `VEOVEO_PUBLIC_ORIGIN` unset | Checks. The submission names no webhook. Media checks the prediction's status endpoint, the provider's `urls.get`, first after 3 seconds, then every 3 seconds growing by a quarter each time, at most 40 times before the deadline. A status endpoint off the provider's origin is never called. |

Both end at the deadline, two hours after dispatch by default. An expired wait never
proves failure: a later report still settles the provider's job, and the run records
itself unresolved.

### Outputs

Each output is one durable step. Media fetches the output from its URL and streams it
into Artifact through `ArtifactClient`, bound to the run's Task, so the artifact belongs
to the run's caller and records Media as the acting server and the Task as its
provenance. The fetch reaches only allowed hosts, the provider's own by default, and is
bounded by `MEDIA_OUTPUT_MAX_BYTES` and a 15-minute timeout. Media follows at most three
redirects, each to an allowed host over HTTP or HTTPS; a redirect anywhere else fails
the output. No output is held in
memory whole. The upload carries the idempotency key `media:{task}:output:{index}`, the
caller's labels and metadata naming the Task, the prediction, the model and the index,
and waits for Artifact to verify the bytes. A result links each output by its address,
`artifact://artifacts/{artifact_id}`, and never carries a download URL.

### Costs

The estimate is recorded before the dispatch. After the outcome, a second operation
checks the provider's billing records for the prediction on a bounded schedule, and
their net amount becomes the run's actual cost. A bill that does not appear before the
billing deadline leaves the estimate as the run's cost. Every Task view carries its
costs in its `ai.veoveo/usage` metadata.

### Cancellation

A run cancelled before a worker claims it never reaches the provider. A claimed run
stops at the check before dispatch, or, once dispatched, sends the provider its
deletion through the wait and keeps the provider's receipt. A cancelled run keeps its
estimate.

## Provider

The provider's identity stays in `src/provider/`. Its address is required
configuration with no default, so every installation and test names the provider it
calls; tests and the local installation run the fake provider from the `testing`
feature. Every call carries a timeout: 60 seconds for the registry, 30 for a
submission, 15 for a status read and a billing search, and 10 for a deletion. No error
names a URL, because a submission's URL carries the callback's token.

## Catalog

The catalog is an immutable snapshot of the provider's registry, read at startup and
every hour, and replaced whole when a refresh reads a valid registry: unique models and
finite prices. A failed refresh keeps the current snapshot. Catalog cursors are bound
to the snapshot's SHA-256, the filters and the limit, so a cursor never pages another
registry. Probes never call the provider.

## Persistence

| Table | Holds | Retention |
|---|---|---|
| `media.prediction` | One row per run: tenant, Task, owner, labels, model, the provider's prediction and status endpoint, status, error, stored outputs and times | Until 30 days after the run ended, when the sweep deletes it |

Core's Task runtime holds the Task, its steps, its operations and their events; core's
usage ledger holds its costs.

## Authorization

| Permission | Resource | Access | Risks |
|---|---|---|---|
| `media.models.read` | `media.model` | read | |
| `media.predictions.read` | `media.prediction` | read | |
| `media.generations.run` | `media.generation` | write | billed |

Media declares `artifact.artifacts.read` and `artifact.artifacts.write` as its uses of
Artifact. A run's calls to Artifact carry only those of the two the caller holds.

## Registration

Media registers its identity and the URL where the Gateway reaches it,
`{VEOVEO_PUBLIC_BASE_URL}/media/mcp`, with the requirement table and its uses of
Artifact. Its key in `VEOVEO_KEY_DIRECTORY` keeps its namespace across restarts and
signs the client assertions of its token exchanges.

## Deployment

| Variable | Meaning |
|---|---|
| `VEOVEO_PUBLIC_BASE_URL`, `VEOVEO_PORT` | Where the Gateway reaches Media, and the port, 8802 by default |
| `VEOVEO_GATEWAY_ISSUER`, `VEOVEO_REGISTRATION_SECRET`, `VEOVEO_KEY_DIRECTORY` | Registration with the Gateway |
| `MEDIA_PROVIDER_URL` | The provider's API origin; required |
| `MEDIA_PROVIDER_API_KEY`, `MEDIA_PROVIDER_WEBHOOK_SECRET` | The provider's API key and webhook secret |
| `VEOVEO_PUBLIC_ORIGIN` | The installation's public `https` origin, which joins Media's SDK deployment: Media accepts requests for its host, the provider reaches Media's callback path under it, and setting it selects events completion |
| `MEDIA_OUTPUT_HOSTS` | The hosts outputs may come from, beside the provider's own |
| `MEDIA_OUTPUT_MAX_BYTES`, `MEDIA_GENERATION_TIMEOUT_SECS`, `MEDIA_RUN_RETENTION_DAYS`, `MEDIA_OUTPUT_RETENTION_DAYS` | Output bound, generation deadline, run retention and output retention |

## Contract Compliance

| Area | State |
|---|---|
| Helm chart | Pending (C15) |
| Installed Discover and list readiness | Pending (C31) |
| Owner naming producers and consumers | Pending (C33) |
| The Studio App | Arrives with the TypeScript SDK |
| Subscription delivery across tenants | A prediction listing's change notifies every listener of `media://predictions`, who rereads only their own runs |

## Verification

| Suite | Covers |
|---|---|
| Unit tests | Identities, resource routes, permissions, webhook signatures, the provider client's address, status-endpoint and redaction rules, the catalog's pages, cursors and input checks, the requirement table and tool annotations, prompts and configuration |
| `tests/store.rs` | The `media` schema on PostgreSQL: run states and their first ending, reads by owner and labels, keyset pages and their cursors, and the sweep |
| `tests/run.rs` | Runs on the SDK's in-process mesh against the fake provider, with Artifact in process over a private RustFS: a run stored and billed; a refused input; a forged, repeated and late webhook; a restart while parked; cancellation before and after dispatch; an output redirected off the allowed hosts; a large output in parts; a provider failure; a caller without the run permission; completion through status checks; a fake provider restarted against the same database |
| `tests/database_conformance.rs` | Core's database rules over every statement and the schema |
| `tests/acceptance.rs` | Ignored by default: a run through the Gateway of a running installation against the fake provider, its output read through Artifact, and the member refused the billed run |
| `tests/provider_qualification.rs` | Ignored by default and gated by `MEDIA_PROVIDER_QUALIFICATION=1`: one billed generation with the real provider through the full flow |
