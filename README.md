# veoveo-mcp-media

The Veoveo Media MCP server. It generates images, video and audio with a provider's
models: a run is a durable Task that hands the generation to the provider, parks until
the provider reports the outcome, stores each output in
[Artifact](https://github.com/VeoveoAI/veoveo-mcp-artifact) for the run's caller and
records the run's cost in core's usage ledger. The design is
[`crates/veoveo-mcp-media/DESIGN.md`](crates/veoveo-mcp-media/DESIGN.md).

## Contents

| Path | Purpose |
|---|---|
| `crates/veoveo-mcp-media` | The `contract` types, the hosted server and the fake provider behind `testing` |
| `crates/veoveo-mcp-media/migrations` | The `media` schema, applied in Media's own PostgreSQL schema at startup |
| `.sqlx/` | sqlx's checked metadata for every query the workspace compiles, Artifact's included |
| `Dockerfile` | The `dev` environment, the `check` gate, the release `build` and the `runtime` image |

## Using The Contract

A consumer depends on the crate with `default-features = false, features = ["contract"]`
by Git revision. `MediaProvider` names the server, and `ModelsRequest`,
`ModelSchemaRequest` and `RunRequest` are `ToolCall`s with typed results, so a server
that declares uses of Media's permissions calls Media through the Hub with the SDK's
`HubClient`; `run` is a `TaskToolCall` whose handle resolves with the
`GenerationResult`.

## Database

The crate owns one table, `media.prediction`, one row per run. Every statement is a
`.sql` file under `src/store/` that sqlx checks at compile time. The tests compile
Artifact's queries too, so after changing a query or a migration, regenerate `.sqlx`
against a database with core's kernel migrations, Artifact's migrations in its schema
and Media's migrations in its schema applied, from an empty target directory:

```sh
CARGO_TARGET_DIR=<empty directory> cargo sqlx prepare --workspace -- --all-targets --all-features
```

## Running

The server takes the registration settings every server takes (`VEOVEO_PUBLIC_BASE_URL`,
`VEOVEO_HUB_ISSUER`, `VEOVEO_REGISTRATION_SECRET`, `VEOVEO_KEY_DIRECTORY`) and its
provider settings: `MEDIA_PROVIDER_URL`, which has no default, `MEDIA_PROVIDER_API_KEY`
and `MEDIA_PROVIDER_WEBHOOK_SECRET`. The installation's public origin,
`VEOVEO_PUBLIC_ORIGIN` such as `https://veoveo.example.com`, holds every server under
its own path and selects webhook completion; without it a run sends no webhook and
checks the provider's status endpoint.
It listens on port 8802 and serves MCP at `{public_base_url}/media/mcp` and the
provider's callbacks at `{public_origin}/media/operations/{operation_id}/{token}`.

## Gate

```sh
docker build --target check .
```

The `check` stage checks the `.sqlx` metadata against a migrated PostgreSQL 18.6
database, then runs rustfmt, clippy with warnings denied, cargo-deny, cargo-hack on each
feature and the nextest `ci` profile with every feature on. The run tests start a
private PostgreSQL, a private SeaweedFS and the fake provider per test. The gate never
reaches the real provider.

## Development Environment

```sh
docker build --target dev -t veoveo-mcp-media:dev .
```

The `dev` image contains Rust, PostgreSQL and SeaweedFS for component tests. The
`runtime` target builds only the production Media binary, installs certificate
roots and owns `/var/lib/veoveo/media/keys` as UID 10001. The separate
`fake-provider` target builds the existing test fixture with `testing`; it does
not enable that feature in the production build.

```sh
docker build --target runtime -t veoveo-mcp-media:runtime .
docker build --target fake-provider -t veoveo-mcp-media:fake-provider .
```

## Native Installation

Native Media uses the qualified transport and private placement below. The
expanded lifecycle probes require separate deployed evidence; source and component
checks alone do not qualify them. Follow the installation-owned setup and policies in
[veoveo-template-installation](../veoveo-template-installation/openshell/MEDIA.md)
and the [Media execution plan](../veoveo-docs/OPENSHELL_HARDCUT_PLAN.md#media-native-integration-plan).
Endpoint, provider and placement configuration belongs to that trusted
installation, retaining the caller's existing Work Context.

The service is private `vv-services/media`, listening on port 8802 and
registering the logical endpoint `http://media.internal:8802/media/mcp` with Hub.
Hub's startup-approved binding uses native authenticated forwarding; Media has no
public MCP exposure. Registration uses a native opaque initial-access reference,
and Media's own signing key persists in its UID 10001 state volume. Inventory and
adopt an existing registration key before generating another. The service applies
its own schema migrations using the login issued at registration.

For the native fake-provider slice, leave `VEOVEO_PUBLIC_ORIGIN` unset. The separate
`vv-services/media-provider` fixture listens on loopback port 8803 behind its native
service exposure in `bearer-passthrough` mode. Set
`MEDIA_FAKE_PROVIDER_ADVERTISED_BASE` (CLI `--advertised-base`) to the exact returned
HTTPS origin, independent of `MEDIA_FAKE_PROVIDER_LISTEN`. It must be an HTTP(S)
origin without credentials, path, query, fragment or an unspecified IP. Its status
and output URLs use this origin. In-process tests omit the advertised base and use
the bound loopback address. Media's `MEDIA_PROVIDER_URL` and output admission must
agree with the selected fixture origin, and native policy admits only the service
binary's required provider requests and byte transfers.

Keep provider credentials in the trusted service/native credential configuration.
`MEDIA_PROVIDER_WEBHOOK_SECRET` remains required even in status-check mode, although
no public callback is used. Neither the acceptance caller nor an author/build job
receives a real provider credential, registration key or S3 signing key. Media
stores outputs through Artifact's Task-bound SDK client; it transfers bytes directly
to the signed S3 endpoint with every signed header unchanged. Verify certificate
roots, authority and that endpoint from the actual sandboxes.

## Acceptance

`tests/acceptance.rs` keeps qualification logic in Rust. The ordinary fake-provider
suite checks catalog/schema discovery, the exact Hub billed-permission refusal for
an ordinary member, Task-only invocation, acknowledged Working then pushed
Completed, small and greater-than-20-MiB outputs, Artifact delegation/provenance and
exact saved bytes, measured estimate/actual usage, and cancellation while a quote is
held and after dispatch. A pushed completion is observed without `tasks/get`
between the opening Working notification and the terminal notification.

Use the installation's distinct member and billed-client opaque references.
The existing `ADMIN` input selects that eligible billed test identity; it need not
have administrative risks. The caller needs `media.generations.run` and Artifact
read/write delegation. The member retains browsing and billed-run denial.
Select environment variables containing references; do not copy signing keys:

```sh
VEOVEO_ACCEPTANCE_HUB=https://vv-control--hub--http.openshell.localhost:17670 \
VEOVEO_ACCEPTANCE_MEMBER_BEARER_ENV=HUB_MEMBER_REFERENCE \
VEOVEO_ACCEPTANCE_ADMIN_BEARER_ENV=HUB_ADMIN_REFERENCE \
VEOVEO_ACCEPTANCE_MEDIA_FIXTURE=https://vv-services--media-provider--http.openshell.localhost:17670 \
VEOVEO_ACCEPTANCE_BILLED_PRINCIPAL='https://auth.localhost:8443/realms/veoveo#vv-hub-billed' \
cargo nextest run --locked --features testing --test acceptance --run-ignored only \
  -E 'not (test(real_provider) or test(restart_persistence_) or test(a_running_generation_recovers_after_media_and_hub_restart))' \
  --test-threads 1
```

Inside the native qualification sandbox, invoke the built test binary at its
policy-approved path with those same nonsecret selectors and provider-injected
reference variables:

```sh
/sandbox/bin/media-acceptance --ignored --test-threads 1 --nocapture \
  --skip real_provider --skip restart_persistence_ \
  --skip a_running_generation_recovers_after_media_and_hub_restart
```

`VEOVEO_ACCEPTANCE_WORK_CONTEXT` defaults to `local` and
`VEOVEO_ACCEPTANCE_LABELS` to the JSON array `[]`. Provenance is checked against
these explicit expected inputs and the billed principal, rather than guessed from
an opaque token. Signed download JSON TextContent must equal `structuredContent`;
the test parses that text, then sends the exact GET URL and every signed header and
checks bytes and SHA-256 without logging the URL. Tenant isolation remains a
component-test claim in this one-tenant installation.

The fake is testing-only. Its standalone binary checks the exact bearer configured
by `MEDIA_FAKE_PROVIDER_API_BEARER` (default public test value
`fake-generation-key`) on `/api/v3/**`. In-process fixtures can omit that check.
This value is a fixture input, never a real provider key. Output GETs and controls
are unaffected. The native service receives its provider credential through
credential mediation; acceptance receives no provider credential. The controls
below are admitted only to the trusted qualification executable, outside Media's
`/api/v3/**` and `/outputs/**` policy:

| Test-only route | Input / observation |
|---|---|
| `GET /testing/state` | Request counts and prediction IDs/status; no prompts, URLs or keys |
| `POST /testing/quotes` | `{"hold":true}` or `{"hold":false}`; watch-based quote response barrier |
| `POST /testing/delivery` | `{"automatic":false}` holds future outcomes for explicit completion |
| `POST /testing/predictions/{id}/complete` | `{"deliver":false}` finishes status; `true` also uses the existing signed webhook delivery |

Run all controlled cases serially. They use bounded state predicates, not assumed
provider timing. Submission/output counts independently prove no duplicate work.
The caller API does not expose an operation's cancellation receipt: the component
case checks it through the Task runtime; deployed qualification requires a trusted
operator's read-only receipt check using the printed Media Task ID. Do not add
cross-schema queries or a product endpoint to the acceptance client.

The deployed cancellation cases do not force a later successful provider outcome.
`FakeProvider::finish` changes only `processing` predictions and leaves a cancelled
prediction cancelled. The existing component cancellation case posts that cancelled
state again as a later signed event and checks that the Task stays Cancelled; it
does not simulate a late success after cancellation. No late-success deployment
claim follows from these cases.

For in-flight recovery, run only
`a_running_generation_recovers_after_media_and_hub_restart` with `--ignored
--exact --nocapture`. It prints and flushes the Working Task/prediction, closes its
connection and waits on native Exec stdin. The trusted operator proves the old
Media and Hub processes stopped, restarts the same approved sandboxes with retained
state/keys, verifies readiness and identities, then sends exactly `continue` plus
LF. The same Task must still be Working and the provider must still hold the same
single pending prediction. A fresh acknowledged subscription opens on Working;
only then does the test finish the provider and require pushed Completed, one
output and the retained result. A missing marker times out; early completion is
inconclusive and fails. The runner has no management credentials.

Terminal persistence is separate: run `restart_persistence_prepare` alone, retain
its standard `VEOVEO_MEDIA_RESTART_TASK_ID`, perform independently verified native
Media/Hub restarts, then pass that variable to `restart_persistence_verify` alone.
The verifier checks the retained Task/prediction/result, Artifact metadata and exact
bytes, plus a newly opened subscription. Operator evidence must separately show
unchanged registration public identity/key and approved volumes. This is terminal
persistence, not proof of in-flight recovery.

The Task's frozen result carries the completion-result construction time from
[`run.rs`](crates/veoveo-mcp-media/src/run.rs). Prediction/result resources use the
persisted Task lifecycle completion time through
[`predictions.rs`](crates/veoveo-mcp-media/src/predictions.rs) and
[`resources.rs`](crates/veoveo-mcp-media/src/mcp/resources.rs). Verification requires
both timestamps, checks the resource against the authoritative prediction, and
compares every other typed result field exactly.

Certificate/name verification stays enabled. A transport error is a failed
operation, not proof of permission refusal. For credential renewal, the trusted
operator records initial expiry and actual verifier leeway, verifies automatic
provider refresh without workload/provider restart, then invokes fresh member and
billed clients through the same still-held opaque references after that boundary.
Existing browsing/permission probes supply these fresh calls; no raw token or
refresh loop belongs in this test. Native refresh may fence existing connections:
retain interrupted-operation evidence and never replay mutations automatically.
Run isolated restart proofs after an applied refresh with measured margin before
the next update. OpenCode/MiMo remains the final bounded qualification only after
deterministic tests, deployed probes and no-model Task interoperability all pass.

## Separate Real Provider Qualification

Real provider calls are ignored by default, bill the account and remain outside
this native fake-provider slice. The isolated `tests/provider_qualification.rs`
case requires `MEDIA_PROVIDER_QUALIFICATION=1`, an explicitly configured
`MEDIA_PROVIDER_URL`, trusted provider credential input and allowed output hosts.
It quotes candidate model inputs before submitting one generation, stores outputs
through Artifact and checks estimate/actual cost.

The separate `tests/acceptance.rs` real-provider case also requires
`MEDIA_PROVIDER_QUALIFICATION=1`, `MEDIA_QUALIFICATION_MODEL` and
`MEDIA_QUALIFICATION_COMPLETION` (`checks` or `events`).
`MEDIA_QUALIFICATION_INPUT` optionally supplies JSON model input. It needs the
admin's own usage view at `/admin/local/usage/daily` as well as MCP, so approve its
native policy separately before invocation:

```sh
cargo nextest run --locked --all-features --test provider_qualification \
  --run-ignored only --no-capture
cargo nextest run --locked --features testing --test acceptance \
  --run-ignored only -E 'test(real_provider)' --no-capture
```

These commands do not provision credentials or grant network access. Public signed
webhook delivery requires its own verified ingress and provider secret, with
`MEDIA_QUALIFICATION_CALLBACK_LISTEN` and
`MEDIA_QUALIFICATION_PUBLIC_ORIGIN` for the isolated provider test. Qualify that
path separately; there is no automatic switch from webhook to status checks.

## License

Apache-2.0. See [LICENSE](LICENSE).
