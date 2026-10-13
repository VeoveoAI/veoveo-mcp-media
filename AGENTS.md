# Agent Instructions

These rules apply to veoveo-mcp-media. The crate's own rules are in
[`crates/veoveo-mcp-media/AGENTS.md`](crates/veoveo-mcp-media/AGENTS.md).

- Use upstream runtimes such as OpenShell and PostgreSQL unmodified. Adapt through
  supported configuration.
- Prefer strong types: typed structs, enums and domain identifiers over raw strings and
  JSON wherever the shape is known.
- MCP comes from [veoveo-sdk-rust](https://github.com/VeoveoAI/veoveo-sdk-rust): hosting, registration with the Gateway,
  the forwarded identity, permission requirements, the documents and contract
  resources, subscriptions, the MCP Tasks adapter and calls to other servers. Depend on
  it by Git revision. veoveo-core holds no MCP; take types, the store, Tasks, usage and audit
  from it.
- A server that calls another server depends on that server's `contract` feature by Git
  revision, declares the permissions it calls with through
  `AuthorizationBuilder::uses`, and calls through `ctx.client()` in a handler or
  `ServerCalls::for_task` in a Task worker, as the SDK's
  [calls to other servers](https://github.com/VeoveoAI/veoveo-sdk-rust/blob/main/crates/veoveo-sdk-rust/DESIGN.md#calls-to-other-servers)
  describe. A server whose tools others call publishes each tool's request as a
  `ToolCall` in its own `contract` feature.
- Run Tasks through the SDK's `TaskWorker`, whose handler receives the Task's `ctx`, as
  the SDK's [Task work](https://github.com/VeoveoAI/veoveo-sdk-rust/blob/main/crates/veoveo-sdk-rust/DESIGN.md#task-work) describes. Wrap work a restarted Task must not
  repeat in `ctx.step`, which returns a finished step's stored result.
- Work handed to an outside provider is an operation. `ctx.operation(..).dispatch`
  records it before the call and gives the provider an idempotency key and a callback
  URL; `ctx.wait` parks the Task until the provider's event reaches the callback route
  that `hosting::operations::operation_callbacks` serves, a status check runs or the
  deadline passes. The server writes only the provider's calls and its
  `ProviderEvents` parser, which checks the provider's signature.
- A server that consumes metered or billed resources declares its usage kinds from a
  closed enum at startup through `UsageLedger::declare`, each with its FOCUS service
  category and unit, records an estimate with `ctx.usage()` before the work and the
  actual usage when the provider reports it. Core's
  [usage ledger](https://github.com/VeoveoAI/veoveo-core/blob/50e72a3a1895d59b4f1eedd2b50cf947fa5d2e72/platform/usage/DESIGN.md) holds the records.
- Audit classes are the server's own, `<slug>.<action>`, each mapped to an OCSF class.
  A record's reason is a code the server defines and its target a URI in the server's
  own scheme, as core's
  [audit record contract](https://github.com/VeoveoAI/veoveo-core/blob/50e72a3a1895d59b4f1eedd2b50cf947fa5d2e72/platform/audit/contract/DESIGN.md#outcome-and-reason)
  describes.
- The server owns one PostgreSQL schema named after its slug and applies the crate's
  `migrations/` there at startup. Never edit an applied migration; add a new one.
- Every SQL statement is a `.sql` file beside the code that runs it, run through sqlx's
  checked `query_file*!` macros. Builds read the committed `.sqlx` metadata, because
  `.cargo/config.toml` sets `SQLX_OFFLINE`. After changing a query or a migration,
  regenerate the metadata from an empty target directory.
- Statements and tables follow the server schema rules of veoveo-core's
  [database design](https://github.com/VeoveoAI/veoveo-core/blob/50e72a3a1895d59b4f1eedd2b50cf947fa5d2e72/docs/DATABASE.md),
  which `crates/veoveo-mcp-media/tests/database_conformance.rs` checks. A statement binds its tenant, bounds its
  rows and reaches them through an index in a generic plan. A table has a primary key,
  an index whose leading columns are each foreign key's, and a
  `COMMENT ON TABLE … IS 'Retention: …'`. A statement that cannot comply carries a
  `-- veoveo:` marker that states its reason.
- Page a listing with core's `PageSize` and a `KeysetCursor` from a `CursorScope`
  bound to the listing and the tenant, with one statement for the first page and one
  for the pages after a cursor. Do not write a `$cursor IS NULL OR …` predicate, which
  loses the index seek in a cached generic plan.
- The real generation provider is reached only by the ignored qualification test, run by
  hand with `MEDIA_PROVIDER_QUALIFICATION=1`; every other test and the local installation
  use the fake provider. Never print the provider's API key or webhook secret.
- Keep test logic in Rust and tool behavior in each tool's own configuration. Do not add
  a task runner or shell scripts that hold test logic.
- Pin exact versions and image digests. When changing a pin, verify the latest stable
  release at its source.
- Run `docker build --target check .` on macOS and on Linux before pushing.
- Names, identifiers and formats follow the
  [naming and format conventions](https://github.com/VeoveoAI/veoveo-core/blob/main/docs/CONVENTIONS.md),
  and MCP names follow the SDK's
  [MCP naming conventions](https://github.com/VeoveoAI/veoveo-sdk-rust/blob/main/docs/CONVENTIONS.md).
  Where a standard governs a name or format, follow it; extend a standard only through
  its own extension mechanism. URIs, paths and identifiers carry no version segment such
  as `/v1`; releases carry versions.
- Commit small, coherent changes. Commits carry no AI or tool attribution.
