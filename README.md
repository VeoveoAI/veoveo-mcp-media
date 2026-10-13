# veoveo-template-mcp-rust

This template holds the starting files for a Veoveo MCP server written in Rust. A new
component repository copies them once and owns them from then on. A server serves MCP
through [veoveo-sdk-rust](https://github.com/VeoveoAI/veoveo-sdk-rust), which registers it with the Gateway and verifies the
identity the Gateway forwards, and stores its state through
[veoveo-core](https://github.com/VeoveoAI/veoveo-core). Components couple
only through contracts, conformance suites and the `rust-version` of the crates they
compile from other repositories, so each repository keeps its own image, toolchain
and gate configuration current.

## Contents

| Path | Purpose |
|---|---|
| `Dockerfile` | The `dev` environment, the `check` gate, the release `build` and the `runtime` image |
| `openshell/policy.yaml` | The OpenShell policy for the development environment |
| `env/profile.d/rust.sh` | Toolchain variables for login shells inside the environment |
| `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml` | The pinned toolchain and code rules |
| `deny.toml` | Dependency licenses, advisories, bans and allowed sources |
| `.config/nextest.toml` | Test profiles for local runs and machine-readable results |
| `.cargo/config.toml` | `SQLX_OFFLINE`, so every build reads the committed query metadata |
| `.sqlx/` | sqlx's checked metadata for every query the workspace compiles |
| `crates/` | The component's crates; the template ships one with an example table and listing |
| `crates/*/migrations/` | The server's schema, applied in its own PostgreSQL schema at startup |

## Database

The template's crate owns one tenant-scoped table, `template.item`, and one listing
over it. The listing pages by key with veoveo-core's `PageSize` and a `KeysetCursor`
bound to the tenant, through a first-page and a next-page statement. Every statement is
a `.sql` file that sqlx checks at compile time against the migrated schema.

The statements and tables follow the server schema rules of veoveo-core's
[database design](https://github.com/VeoveoAI/veoveo-core/blob/50e72a3a1895d59b4f1eedd2b50cf947fa5d2e72/docs/DATABASE.md):
tenant predicates, row bounds, index use under generic plans, and a primary key, an
index behind every foreign key and a `Retention:` comment on every table.
`tests/database_conformance.rs` checks them against a fresh database. Tests need the
PostgreSQL 18 server programs on `PATH`; each test clones its own database from a
migrated template.

After changing a query or a migration, regenerate `.sqlx` against a database with the
kernel migrations of the pinned veoveo-core and the crate's migrations applied, from a
target directory with no earlier build:

```sh
CARGO_TARGET_DIR=<empty directory> cargo sqlx prepare --workspace -- --all-targets --all-features
```

The metadata also covers the queries of the core crates, and prepare records only the
crates it compiles. The `check` stage of the `Dockerfile` builds that database and fails
when the metadata is stale.

## Tasks, providers, usage and audit

The template's crate runs no Tasks. A server that does runs them through the SDK's
`TaskWorker`, where `ctx.step` keeps a restarted Task from repeating finished work and
`ctx.operation` with `ctx.wait` parks a Task on an outside provider until the
provider's event reaches the server's callback route; the SDK's
[Task work](https://github.com/VeoveoAI/veoveo-sdk-rust/blob/main/crates/veoveo-sdk-rust/DESIGN.md#task-work) describes both. Metered or billed work records an estimate
and the actual usage through `ctx.usage()` against usage kinds the server declares, in
core's [usage ledger](https://github.com/VeoveoAI/veoveo-core/blob/50e72a3a1895d59b4f1eedd2b50cf947fa5d2e72/platform/usage/DESIGN.md). Audit classes, reason codes and
target URIs are the server's own, under its slug, as core's
[audit record contract](https://github.com/VeoveoAI/veoveo-core/blob/50e72a3a1895d59b4f1eedd2b50cf947fa5d2e72/platform/audit/contract/DESIGN.md) describes.

## Start a component

Create the repository from this template and rename the crate to the repository name.
In the `Dockerfile`, set `COMPONENT` and the `ENTRYPOINT` path to that name: the crate,
binary, image and image title all use the repository name. The server's slug names its
schema: replace `template` in `server()`, the crate's `sqlx.toml`, its migrations and
statements, the `Dockerfile`'s `ensure_server` call and the binary path in
`openshell/policy.yaml`, then regenerate `.sqlx`. Build the
runtime image with its version and source revision:

```sh
docker build --target runtime -t <repository>:<version> \
  --build-arg VERSION=<version> --build-arg REVISION="$(git rev-parse HEAD)" .
```

## Gate

```sh
docker build --target check .
```

The `check` stage checks the `.sqlx` metadata against a migrated PostgreSQL 18.6
database, then runs rustfmt, clippy with warnings denied, cargo-deny, cargo-hack on each
feature and the nextest `ci` profile. It runs the same way on macOS and Linux.

## Development environment

Build the `dev` image on the host and create an OpenShell sandbox from it:

```sh
docker build --target dev -t <repository>:dev .
openshell sandbox create --name <sandbox> --from <repository>:dev \
  --policy openshell/policy.yaml --detach -- sleep infinity
```

The policy lets the component reach the installation's PostgreSQL server at
`host.openshell.internal:5432`. OpenShell pins each policy-listed executable to the
SHA-256 it first observes, so the component runs from a content-addressed copy under
`/sandbox/run`.

OpenShell limits sandbox names to 19 characters, so sandboxes and other runtime
identifiers use a short form: `vv-` plus a short kind and the component name. The
kinds are `srv` for services, `mcp` for MCP servers, `rt` for runtimes, `app`,
`sdk`, `tpl` for templates, `demo` for showcases and `qual` for qualification suites,
and repositories without a kind use `vv-<name>`. For example, `veoveo-service-gateway`
runs as `vv-srv-gateway` and `veoveo-mcp-time` as `vv-mcp-time`. Repositories,
crates, binaries and images keep their full names.

Inside the sandbox, the `sandbox` user owns the Rust toolchain and Cargo home, so
rustup and Cargo install components, targets and crates as needed. A coding agent on
the host works on the source through OpenShell SSH: it pushes commits into a working
tree at `/sandbox/src` that accepts pushes with `receive.denyCurrentBranch=updateInstead`,
then builds and tests inside the sandbox.

## License

Apache-2.0. See [LICENSE](LICENSE).
