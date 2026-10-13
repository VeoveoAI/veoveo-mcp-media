-- A page of a tenant's items after a cursor's key. A separate statement for the first
-- page keeps this bound an index condition in a cached generic plan.
SELECT item_key AS "item_key: ItemId", name
FROM template.item
WHERE tenant = $1 AND item_key > $2
ORDER BY item_key
LIMIT $3
