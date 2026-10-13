//! The Media MCP server's library. The `contract` feature holds Media's public types and
//! typed calls for consumers; the default `server` feature adds the server, which runs
//! generations as durable Tasks parked on provider operations, stores outputs through
//! Artifact and records usage in core's ledger; the `testing` feature adds the fake
//! provider that tests and the local installation run.

#[cfg(feature = "contract")]
pub mod contract;

#[cfg(feature = "server")]
pub mod catalog;
#[cfg(feature = "server")]
pub mod mcp;
#[cfg(feature = "server")]
pub mod provider;
#[cfg(feature = "server")]
pub mod run;
#[cfg(feature = "server")]
pub mod server;
#[cfg(feature = "server")]
pub mod store;

#[cfg(feature = "contract")]
use veoveo_types::ServerSlug;

/// The component name, taken from the crate name.
pub fn component() -> &'static str {
    env!("CARGO_PKG_NAME")
}

/// The server's slug, which names its schema `media`, its roles, its channel and the
/// provider that consumers call.
#[cfg(feature = "contract")]
pub fn server() -> ServerSlug {
    ServerSlug::parse("media").expect("Media's slug")
}

/// The migrations in `migrations/`, which the server applies in its own schema at
/// startup and which record their history in `media._sqlx_migrations`.
#[cfg(feature = "server")]
pub fn migrator() -> sqlx::migrate::Migrator {
    sqlx::migrate!()
}

#[cfg(test)]
mod tests {
    #[test]
    fn component_is_the_crate_name() {
        assert_eq!(super::component(), "veoveo-mcp-media");
    }
}
