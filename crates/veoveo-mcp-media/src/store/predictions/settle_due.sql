-- veoveo: all tenants (the sweep settles every tenant's runs whose Task ended without them)
-- One batch of runs still open in Media whose Task in core's Task runtime has ended, and
-- of runs created before the cutoff whose Task no longer exists, in Task order.
SELECT
    p.tenant AS "tenant: StoredTenant",
    p.task_id,
    t.status AS "task_status?: TaskStatus",
    t.error_code AS "task_error_code?",
    t.error_message AS "task_error?",
    t.completed_at AS "task_completed_at?"
FROM media.prediction AS p
LEFT JOIN tasks.task AS t ON t.id = p.task_id AND t.tenant_id = p.tenant
WHERE p.completed_at IS NULL
    AND (t.completed_at IS NOT NULL OR (t.id IS NULL AND p.created_at < $1))
ORDER BY p.task_id
LIMIT $2
