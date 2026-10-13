-- Record one item of a tenant.
INSERT INTO template.item (tenant, item_key, name)
VALUES ($1, $2, $3)
RETURNING item_key AS "item_key: ItemId", name
