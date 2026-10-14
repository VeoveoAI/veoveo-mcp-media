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
| `openshell/policy.yaml` | The OpenShell policy for the development environment |

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

Build the `dev` image on the host and create an OpenShell sandbox from it with the
installation's `vv-registration` provider attached:

```sh
docker build --target dev -t veoveo-mcp-media:dev .
openshell sandbox create --name vv-mcp-media --from veoveo-mcp-media:dev \
  --policy openshell/policy.yaml --provider vv-registration --detach -- sleep infinity
```

The policy lets Media reach the Gateway at `host.openshell.internal:8080`, the
installation's PostgreSQL server at port 5432 and its object store at port 9000, where
Media sends each output's bytes to the URLs Artifact signs. The fake provider runs in the
same environment on loopback and needs no policy entry.

### Running Media in the environment

Media and the fake provider share a webhook secret, which Media checks signatures with
in-process, so the installation keeps both provider values in an owner-only file,
`/sandbox/state/media.env`:

```sh
openshell sandbox exec vv-mcp-media -- bash -lc 'mkdir -p /sandbox/state && umask 077 \
  && printf "MEDIA_PROVIDER_API_KEY=fake\nMEDIA_PROVIDER_WEBHOOK_SECRET=whsec_%s\n" \
       "$(head -c 24 /dev/urandom | od -An -tx1 | tr -d " \n")" > /sandbox/state/media.env'
```

OpenShell pins every policy-listed executable path to the SHA-256 it first observes, so
Media and the fake provider run from content-addressed copies under `/sandbox/run`.
Served binaries build into their own target directory, `/sandbox/target-serve`, apart
from `target/`, where the gate and acceptance builds enable test-only dependency
features. Each binary builds alone, right before it is installed: the fake provider with
the `testing` feature it requires, Media without it, so Media never serves a test build.
Start the fake provider in one terminal and Media in another:

```sh
openshell sandbox exec vv-mcp-media -- bash -lc 'cd /sandbox/src \
  && cargo build --locked --target-dir /sandbox/target-serve --features testing \
       --bin veoveo-mcp-media-fake-provider \
  && fake=/sandbox/run/veoveo-mcp-media-fake-provider-$(sha256sum /sandbox/target-serve/debug/veoveo-mcp-media-fake-provider | cut -c1-16) \
  && install -D -m 0755 /sandbox/target-serve/debug/veoveo-mcp-media-fake-provider "$fake" \
  && set -a && . /sandbox/state/media.env && set +a \
  && setsid "$fake" --listen 127.0.0.1:8803'
openshell sandbox exec vv-mcp-media -- bash -lc 'cd /sandbox/src \
  && cargo build --locked --target-dir /sandbox/target-serve --bin veoveo-mcp-media \
  && bin=/sandbox/run/veoveo-mcp-media-$(sha256sum /sandbox/target-serve/debug/veoveo-mcp-media | cut -c1-16) \
  && install -D -m 0755 /sandbox/target-serve/debug/veoveo-mcp-media "$bin" \
  && set -a && . /sandbox/state/media.env && set +a \
  && VEOVEO_PUBLIC_BASE_URL=http://host.openshell.internal:8802 \
     VEOVEO_GATEWAY_ISSUER=http://host.openshell.internal:8080 \
     VEOVEO_KEY_DIRECTORY=/sandbox/media/keys \
     VEOVEO_ALLOW_LOOPBACK_HOSTS=true \
     MEDIA_PROVIDER_URL=http://127.0.0.1:8803 \
     "$bin"'
openshell forward start 8802 vv-mcp-media -d
```

Without a public origin, runs complete through status checks of the fake. An
installation whose tunnel carries its public origin sets `VEOVEO_PUBLIC_ORIGIN` to that
`https` origin, such as `https://veoveo.example.com`; Media then accepts requests for the
origin's host, and the provider posts each outcome to
`https://veoveo.example.com/media/operations/…`.
Media registers before it listens, and the Gateway reaches it as
`http://host.openshell.internal:8802/media/mcp`. Media removes its registration when it
receives SIGTERM or Ctrl-C.

## Acceptance

`tests/acceptance.rs` calls Media through the Gateway as the installation's
`acceptance-admin` and `acceptance-member` callers against the fake provider: the admin
runs the fake image model, reads the stored output through Artifact's signed download,
reads the prediction and finds the run's cost on its Task; the member reads the catalog
and is refused `run`, whose permission carries the `billed` risk.

The tests are ignored by default and compare the stored bytes with the fake provider's,
so they build with the `testing` feature. With the installation running and the callers'
keys copied to `/sandbox/acceptance`, run them from a fresh nextest archive extraction:

```sh
openshell sandbox exec vv-mcp-media -- bash -lc 'cd /sandbox/src \
  && cargo nextest archive --features testing --test acceptance \
       --archive-file /sandbox/run/acceptance.tar.zst \
  && run=/sandbox/run/acceptance-$(date +%s%N) && mkdir -p "$run" \
  && VEOVEO_ACCEPTANCE_GATEWAY=http://host.openshell.internal:8080 \
     VEOVEO_ACCEPTANCE_KEY_DIRECTORY=/sandbox/acceptance \
     cargo nextest run --archive-file /sandbox/run/acceptance.tar.zst \
       --workspace-remap /sandbox/src --extract-to "$run" --run-ignored only \
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
openshell sandbox exec vv-mcp-media -- bash -lc 'cd /sandbox/src \
  && cargo nextest archive --features testing --test acceptance \
       --archive-file /sandbox/run/acceptance.tar.zst \
  && run=/sandbox/run/acceptance-$(date +%s%N) && mkdir -p "$run" \
  && MEDIA_PROVIDER_QUALIFICATION=1 MEDIA_QUALIFICATION_MODEL=<model> \
     MEDIA_QUALIFICATION_COMPLETION=<events or checks> \
     VEOVEO_ACCEPTANCE_GATEWAY=http://host.openshell.internal:8080 \
     VEOVEO_ACCEPTANCE_KEY_DIRECTORY=/sandbox/acceptance \
     cargo nextest run --archive-file /sandbox/run/acceptance.tar.zst \
       --workspace-remap /sandbox/src --extract-to "$run" --run-ignored only \
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

Inside the Media environment, the API key reaches the test as an OpenShell placeholder
that resolves only on requests to the provider. Create a provider whose credential is
`MEDIA_PROVIDER_API_KEY`, attach it to the environment, and add a policy entry for the
provider's API host and the hosts its outputs come from, the API host with the
provider's credential binding:

```yaml
  media_provider:
    endpoints:
      - host: <provider API host>
        port: 443
        protocol: rest
        enforcement: enforce
        rules:
          - allow: {method: GET, path: "/api/v3/**"}
          - allow: {method: POST, path: "/api/v3/**"}
        credential_binding:
          provider: <the provider's OpenShell name>
      - host: <provider output host>
        port: 443
    binaries:
      - path: /sandbox/run/qualification-*/**
```

Then run the test from a fresh nextest archive extraction:

```sh
openshell sandbox exec vv-mcp-media -- bash -lc 'cd /sandbox/src \
  && cargo nextest archive --all-features --test provider_qualification \
       --archive-file /sandbox/run/qualification.tar.zst \
  && run=/sandbox/run/qualification-$(date +%s%N) && mkdir -p "$run" \
  && MEDIA_PROVIDER_QUALIFICATION=1 MEDIA_PROVIDER_URL=https://<provider API host> \
     MEDIA_OUTPUT_HOSTS=<provider output host> \
     cargo nextest run --archive-file /sandbox/run/qualification.tar.zst \
       --workspace-remap /sandbox/src --extract-to "$run" --run-ignored only --no-capture'
```

Without a public callback, the run completes through status checks. To cover webhook
delivery too, the test listens for forwarded webhooks on
`MEDIA_QUALIFICATION_CALLBACK_LISTEN` (`127.0.0.1:8899` by default). Forward that port
to the host with `openshell forward start 8899 vv-mcp-media -d`, open a temporary public
tunnel on the host to `http://127.0.0.1:8899`, and set
`MEDIA_QUALIFICATION_PUBLIC_ORIGIN` to the tunnel's origin, which stands in for the
installation's public origin such as `https://veoveo.example.com`, and
`MEDIA_PROVIDER_WEBHOOK_SECRET` to the account's webhook secret, from an owner-only file
in the environment. Close the tunnel when the test ends. The test prints the model, the
prediction and the run's cost, never the key or the secret.

## License

Apache-2.0. See [LICENSE](LICENSE).
