//! Media's SQL names only the `media` schema. Core state, such as a run's Task, comes
//! through veoveo-sdk-rust and core's runtime API.

#[test]
fn sql_names_only_the_media_schema() {
    veoveo_sdk_rust::testing::assert_sql_confined(env!("CARGO_MANIFEST_DIR"), "media");
}
