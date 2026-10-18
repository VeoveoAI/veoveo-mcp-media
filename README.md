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

Native Media adoption is in preparation; these source changes claim no deployed
qualification. Follow the installation-owned setup and policies in
[veoveo-template-installation](../veoveo-template-installation/openshell/README.md)
and the [Media execution plan](../veoveo-docs/OPENSHELL_HARDCUT_PLAN.md#media-native-integration-plan).
Endpoint, provider and placement configuration belongs to that trusted
installation, retaining the caller's existing Work Context.

The planned service is private `vv-services/media`, listening on port 8802 and
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

`tests/acceptance.rs` has two ignored fake-provider cases: an eligible admin caller
runs the image model, downloads its Artifact output, reads the prediction and checks
Task usage; an ordinary member browses models and is refused the billed run.
`media.generations.run` is a billed permission, so native qualification must grant
it deliberately to the selected test caller, together with Artifact delegation.
Plain tool invocation is refused: `run` requires MCP Tasks.

Use prepared native member/admin references with one explicit endpoint provider
binding, as the installation's qualification profile permits. Select the variables
containing those references; do not copy private caller signing keys:

```sh
VEOVEO_ACCEPTANCE_HUB=https://vv-control--hub--http.openshell.localhost:17670 \
VEOVEO_ACCEPTANCE_MEMBER_BEARER_ENV=HUB_MEMBER_REFERENCE \
VEOVEO_ACCEPTANCE_ADMIN_BEARER_ENV=HUB_ADMIN_REFERENCE \
cargo nextest run --locked --features testing --test acceptance --run-ignored only \
  -E 'not test(real_provider)' --test-threads 1
```

Run this inside the policy-approved native qualification sandbox after the
installation supplies both reference variables and verified trust roots. Selectors
and reference values must be distinct; the retired
`VEOVEO_ACCEPTANCE_KEY_DIRECTORY` input is rejected. Member-only input is sufficient
for the member case. Certificate/name verification stays enabled. A transport error
is a failed operation, not proof of a Hub permission refusal. Native credential
refresh may fence an existing connection; retain that evidence and make explicit
fresh-client calls rather than replaying mutations automatically.

These two cases do not prove deployed cancellation, provider no-resubmission or
native restart recovery. The component gate has those lifecycle regressions; the
execution plan requires independent deployed proofs before qualifying Media.
OpenCode/MiMo is reserved for the last bounded qualification after deterministic
checks pass, with Task interoperability preflighted without model calls.

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
