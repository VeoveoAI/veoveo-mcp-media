//! Media's SQL names only the `media` schema. Core state, such as a run's Task, comes
//! through veoveo-hub-sdk and core's runtime API.

#[test]
fn sql_names_only_the_media_schema() {
    veoveo_hub_sdk::testing::assert_sql_confined(env!("CARGO_MANIFEST_DIR"), "media");
}
