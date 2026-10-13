-- The first page of a tenant's items in key order, with one row of lookahead.
SELECT item_key AS "item_key: ItemId", name
FROM template.item
WHERE tenant = $1
ORDER BY item_key
LIMIT $2
