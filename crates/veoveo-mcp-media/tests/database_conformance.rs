//! The crate's queries and its schema against the server schema rules of veoveo-core's
//! `docs/DATABASE.md`: tenant predicates, row bounds, index use under generic plans, and
//! primary keys, foreign-key indexes and retention comments. The check reads the
//! queries from the workspace's `.sqlx` metadata and needs the PostgreSQL 18 server
//! programs on `PATH`.
use veoveo_test_fixtures::database::Conformance;

#[tokio::test]
async fn queries_and_schema_follow_the_database_rules() {
    Conformance::new()
        .server(&veoveo_mcp_media::server(), veoveo_mcp_media::migrator())
        .queries_from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.sqlx"))
        .sources(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))
        .check()
        .await;
}
