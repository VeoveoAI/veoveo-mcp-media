//! A tenant's items, the template's example listing. Each statement is a `.sql` file
//! beside this module, run through sqlx's checked `query_file*!` macros. The listing
//! pages by key with core's `PageSize` and a `KeysetCursor` bound to the tenant, and it
//! has a first-page and a next-page statement so the key bound stays an index range.
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use veoveo_platform_store::{
    CursorError, CursorScope, KeysetCursor, Page, PageSize, PlatformStore, TenantId,
};

/// The most items one page holds.
pub const MAX_ITEMS_PER_PAGE: u32 = 100;

/// A page size of the item listing, from 1 to [`MAX_ITEMS_PER_PAGE`].
pub type ItemPageSize = PageSize<MAX_ITEMS_PER_PAGE>;

/// The position after the last item of a page, bound to the tenant that read it.
pub type ItemCursor = KeysetCursor<ItemId>;

/// An item's key: a UUIDv7, so key order is the order the server created items in.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, sqlx::Type,
)]
#[serde(transparent)]
#[sqlx(transparent)]
pub struct ItemId(Uuid);

impl ItemId {
    /// A fresh key.
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for ItemId {
    fn default() -> Self {
        Self::new()
    }
}

/// One item of a tenant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub item_key: ItemId,
    pub name: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ItemsError {
    #[error(transparent)]
    Cursor(#[from] CursorError),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

/// The listing's cursors bind to the listing and the tenant. It takes no other filter.
fn scope(tenant: TenantId) -> Result<CursorScope, CursorError> {
    CursorScope::new("template.items", tenant, &())
}

/// Record an item for `tenant`.
pub async fn create_item(
    store: &PlatformStore,
    tenant: TenantId,
    name: &str,
) -> Result<Item, ItemsError> {
    Ok(sqlx::query_file_as!(
        Item,
        "src/items/insert_item.sql",
        tenant as TenantId,
        ItemId::new() as ItemId,
        name,
    )
    .fetch_one(store.pool())
    .await?)
}

/// One page of `tenant`'s items in key order, after `cursor` when the caller returns
/// the previous page's. A cursor that another tenant's listing issued is refused.
pub async fn items_page(
    store: &PlatformStore,
    tenant: TenantId,
    cursor: Option<&str>,
    size: ItemPageSize,
) -> Result<Page<Item, ItemCursor>, ItemsError> {
    let scope = scope(tenant)?;
    let rows = match cursor {
        None => {
            sqlx::query_file_as!(
                Item,
                "src/items/item_page_first.sql",
                tenant as TenantId,
                size.fetch_limit(),
            )
            .fetch_all(store.pool())
            .await?
        }
        Some(wire) => {
            let after: ItemCursor = scope.parse(wire)?;
            sqlx::query_file_as!(
                Item,
                "src/items/item_page_next.sql",
                tenant as TenantId,
                *after.position() as ItemId,
                size.fetch_limit(),
            )
            .fetch_all(store.pool())
            .await?
        }
    };
    Ok(size
        .page(rows, |last| last.item_key)
        .try_map_next(|position| scope.cursor(position))?)
}
