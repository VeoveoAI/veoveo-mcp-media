-- veoveo: all tenants (the sweep walks every tenant's runs past their retention)
-- One batch of runs recorded before the cutoff, oldest first, after the previous batch's
-- last run. The first batch's bound precedes every run, so the keyset stays an index
-- range in a generic plan.
SELECT tenant AS "tenant: StoredTenant", task_id, created_at
FROM media.prediction
WHERE created_at < $1
    AND (created_at, task_id) > (
        coalesce($2::timestamptz, '-infinity'),
        coalesce($3::uuid, '00000000-0000-0000-0000-000000000000')
    )
ORDER BY created_at, task_id
LIMIT $4
