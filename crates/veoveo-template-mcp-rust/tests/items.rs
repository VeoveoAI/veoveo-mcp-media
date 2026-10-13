//! The item listing against a fresh PostgreSQL database migrated as the server's own
//! login. Requires the PostgreSQL 18 server programs on `PATH`.
use std::time::Duration;

use veoveo_platform_store::deterministic_tenant_id;
use veoveo_template_mcp_rust::{
    items::{ItemPageSize, ItemsError, create_item, items_page},
    migrator, server,
};
use veoveo_test_fixtures::store::TestDb;

#[tokio::test]
async fn items_page_in_key_order_and_cursors_stay_with_their_tenant() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let db = TestDb::new().await;
        db.migrate_server(&server(), migrator()).await;
        let store = db.server_store(&server()).await;
        let tenant = deterministic_tenant_id("acme").unwrap();
        let other = deterministic_tenant_id("globex").unwrap();
        let mut created = Vec::new();
        for name in ["first", "second", "third"] {
            created.push(create_item(&store, tenant, name).await.unwrap());
        }
        create_item(&store, other, "elsewhere").await.unwrap();

        // Three items page as two and one, and the last page has no cursor.
        let two = ItemPageSize::new(2).unwrap();
        let first = items_page(&store, tenant, None, two).await.unwrap();
        assert_eq!(first.items, created[..2]);
        let next = first.next.expect("a third item follows");
        let second = items_page(&store, tenant, Some(next.as_str()), two)
            .await
            .unwrap();
        assert_eq!(second.items, created[2..]);
        assert!(second.next.is_none());

        // Another tenant's listing refuses the cursor instead of paging its own rows.
        assert!(matches!(
            items_page(&store, other, Some(next.as_str()), two).await,
            Err(ItemsError::Cursor(_))
        ));
        assert!(ItemPageSize::new(0).is_err());
        assert!(ItemPageSize::new(101).is_err());
    })
    .await
    .expect("the item listing test exceeded 60 seconds");
}
