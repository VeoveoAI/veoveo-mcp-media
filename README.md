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
that declares uses of Media's permissions calls Media through the Gateway with the SDK's
`GatewayClient`; `run` is a `TaskToolCall` whose handle resolves with the
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
`VEOVEO_GATEWAY_ISSUER`, `VEOVEO_REGISTRATION_SECRET`, `VEOVEO_KEY_DIRECTORY`) and its
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

A local installation from
[veoveo-template-installation](https://github.com/VeoveoAI/veoveo-template-installation)
runs this image, under its own layer, as the Compose service `vv-mcp-media`, which
works in the installation's Lore repository at `/sandbox/src`. The environment reaches
the Gateway at `vv-gateway.internal:8080`, PostgreSQL at `vv-postgres.internal:5432` and
the object store at `vv-seaweedfs.internal:9000`, where Media sends each output's bytes
to the URLs Artifact signs. It also joins the network where the installation's edge
answers as `media-provider.internal` for the real provider. The fake provider runs in
the same environment on loopback. Run the commands below from the installation's
directory.

### Running Media in the environment

Media and the fake provider share a webhook secret, which Media checks signatures with
in-process, so the installation keeps both provider values in an owner-only file,
`/sandbox/state/media.env`:

```sh
docker compose exec vv-mcp-media bash -c 'mkdir -p /sandbox/state && umask 077 \
  && printf "MEDIA_PROVIDER_API_KEY=fake\nMEDIA_PROVIDER_WEBHOOK_SECRET=whsec_%s\n" \
       "$(head -c 24 /dev/urandom | od -An -tx1 | tr -d " \n")" > /sandbox/state/media.env'
```

Served binaries build into their own target directory, `/sandbox/target-serve`, apart
from `target/`, where the gate and acceptance builds enable test-only dependency
features. Each binary builds alone, right before it starts: the fake provider with the
`testing` feature it requires, Media without it, so Media never serves a test build.
Start the fake provider in one terminal and Media in another:

```sh
docker compose exec vv-mcp-media bash -lc 'cd /sandbox/src \
  && cargo build --locked --target-dir /sandbox/target-serve --features testing \
       --bin veoveo-mcp-media-fake-provider \
  && set -a && . /sandbox/state/media.env && set +a \
  && exec /sandbox/target-serve/debug/veoveo-mcp-media-fake-provider --listen 127.0.0.1:8803'
docker compose exec vv-mcp-media bash -lc 'cd /sandbox/src \
  && cargo build --locked --target-dir /sandbox/target-serve --bin veoveo-mcp-media \
  && set -a && . /sandbox/state/media.env && set +a \
  && VEOVEO_PUBLIC_BASE_URL=http://vv-mcp-media.internal:8802 \
     VEOVEO_GATEWAY_ISSUER=http://vv-gateway.internal:8080 \
     VEOVEO_KEY_DIRECTORY=/sandbox/media/keys \
     VEOVEO_ALLOW_LOOPBACK_HOSTS=true \
     MEDIA_PROVIDER_URL=http://127.0.0.1:8803 \
     exec /sandbox/target-serve/debug/veoveo-mcp-media'
```

Without a public origin, runs complete through status checks of the fake. An
installation whose tunnel carries its public origin sets `VEOVEO_PUBLIC_ORIGIN` to that
`https` origin, such as `https://veoveo.example.com`; Media then accepts requests for the
origin's host, and the provider posts each outcome to
`https://veoveo.example.com/media/operations/…`.
Media registers before it listens, and the Gateway reaches it as
`http://vv-mcp-media.internal:8802/media/mcp`. Media removes its registration when it
receives SIGTERM or Ctrl-C.

## Acceptance

`tests/acceptance.rs` calls Media through the Gateway as the installation's
`acceptance-admin` and `acceptance-member` callers against the fake provider: the admin
runs the fake image model, reads the stored output through Artifact's signed download,
reads the prediction and finds the run's cost on its Task; the member reads the catalog
and is refused `run`, whose permission carries the `billed` risk.

The tests are ignored by default and compare the stored bytes with the fake provider's,
so they build with the `testing` feature. With the installation running and the callers'
keys copied to `/sandbox/acceptance`, run them:

```sh
docker compose exec vv-mcp-media bash -lc 'cd /sandbox/src \
  && VEOVEO_ACCEPTANCE_GATEWAY=http://vv-gateway.internal:8080 \
     VEOVEO_ACCEPTANCE_KEY_DIRECTORY=/sandbox/acceptance \
     cargo nextest run --features testing --test acceptance --run-ignored only \
       -E "not test(real_provider)"'
```

### Real Provider Through The Installation

One acceptance case bills the real provider once through the whole installation: the
admin runs a model through the Gateway against Media configured for the real provider,
downloads every output through Artifact, finds the run's estimate and actual cost in
the Gateway's daily cost view and the actual one on the Task, and checks that the
outcome arrived the way the configuration chose: `events` through the installation's
public origin, `checks` without one. It is ignored by default and runs only with
`MEDIA_PROVIDER_QUALIFICATION=1`, from the environment's checkout at `/sandbox/src`.
Configure Media for the real provider first, as the installation's README describes,
and name the model to run and the configured completion:

```sh
docker compose exec vv-mcp-media bash -lc 'cd /sandbox/src \
  && MEDIA_PROVIDER_QUALIFICATION=1 MEDIA_QUALIFICATION_MODEL=<model> \
     MEDIA_QUALIFICATION_COMPLETION=<events or checks> \
     VEOVEO_ACCEPTANCE_GATEWAY=http://vv-gateway.internal:8080 \
     VEOVEO_ACCEPTANCE_KEY_DIRECTORY=/sandbox/acceptance \
     cargo nextest run --features testing --test acceptance --run-ignored only \
       -E "test(real_provider)" --no-capture'
```

`MEDIA_QUALIFICATION_INPUT` holds the model's input as JSON when the default short
prompt does not suit the model. The test reads the admin's own cost view at
`/admin/local/usage/daily`, which the installation's `administrative-reads` rule and
Media's policy admit.

## Provider Qualification

`tests/provider_qualification.rs` runs one billed generation with the real provider
through the full flow: the live catalog, the submission, completion, the output stored
in Artifact in process, the reconciled bill and the Task's cost. It is ignored by
default, is never part of the gate, and runs only with `MEDIA_PROVIDER_QUALIFICATION=1`.
It quotes the input on the 20 text-to-image models with the lowest base prices and runs
the one with the lowest quote, unless `MEDIA_QUALIFICATION_MODEL` names another, with a
short prompt unless `MEDIA_QUALIFICATION_INPUT` holds the model's input as JSON. A base
price leaves out what the input's defaults add, so only a quote ranks the models. The
test checks that the quote, the run's estimate, covers the bill.

Inside the Media environment, the test sends the API key's placeholder to the
installation's edge at `media-provider.internal`, which adds the key, and fetches outputs
through the edge's proxy from the hosts `.env` names. Set the provider up in the edge as
the installation's README describes, then run the test with the path of the provider's
API URL after the edge's name:

```sh
docker compose exec vv-mcp-media bash -lc 'cd /sandbox/src \
  && MEDIA_PROVIDER_QUALIFICATION=1 MEDIA_PROVIDER_URL=https://media-provider.internal<API path> \
     MEDIA_PROVIDER_API_KEY=replaced-by-the-edge MEDIA_OUTPUT_HOSTS=<provider output host> \
     cargo nextest run --all-features --test provider_qualification --run-ignored only --no-capture'
```

Without a public callback, the run completes through status checks. To cover webhook
delivery too, the test listens for forwarded webhooks on
`MEDIA_QUALIFICATION_CALLBACK_LISTEN` (`127.0.0.1:8899` by default). Set it to
`0.0.0.0:8899`, open a temporary public tunnel to `http://vv-mcp-media.internal:8899` from
a container on the installation's networks, and set
`MEDIA_QUALIFICATION_PUBLIC_ORIGIN` to the tunnel's origin, which stands in for the
installation's public origin such as `https://veoveo.example.com`, and
`MEDIA_PROVIDER_WEBHOOK_SECRET` to the account's webhook secret, from an owner-only file
in the environment. Close the tunnel when the test ends. The test prints the model, the
prediction and the run's cost, never the key or the secret.

## License

Apache-2.0. See [LICENSE](LICENSE).
