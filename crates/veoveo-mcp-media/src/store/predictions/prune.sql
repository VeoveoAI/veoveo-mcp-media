-- veoveo: all tenants (the sweep prunes every tenant's ended runs in batches)
-- Delete one batch of runs that ended before the cutoff.
WITH due AS (
    SELECT tenant, task_id
    FROM media.prediction
    WHERE completed_at < $1 AND completed_at IS NOT NULL
    ORDER BY completed_at
    LIMIT $2
    FOR UPDATE SKIP LOCKED
)
DELETE FROM media.prediction AS p
USING due
WHERE p.tenant = due.tenant AND p.task_id = due.task_id
