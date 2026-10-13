//! The component's library. The template ships one tenant-scoped table and one keyset
//! listing over it, the smallest shape that the database rules of veoveo-core check,
//! so a new server starts from statements that pass them.

pub mod items;

use veoveo_types::ServerSlug;

/// The component name, taken from the crate name.
pub fn component() -> &'static str {
    env!("CARGO_PKG_NAME")
}

/// The server's slug, which names its schema `template`, its roles and its channel.
/// Rename it with the server, together with the schema in the SQL files.
pub fn server() -> ServerSlug {
    ServerSlug::parse("template").expect("the template's slug")
}

/// The migrations in `migrations/`, which the server applies in its own schema at
/// startup and which record their history in `template._sqlx_migrations`.
pub fn migrator() -> sqlx::migrate::Migrator {
    sqlx::migrate!()
}

#[cfg(test)]
mod tests {
    #[test]
    fn component_is_the_crate_name() {
        assert_eq!(super::component(), "veoveo-template-mcp-rust");
    }
}
