-- One run by its Task, with how its Task ended.
SELECT
    p.tenant AS "tenant: StoredTenant",
    p.task_id,
    p.owner AS "owner: StoredPrincipal",
    p.data_labels,
    p.model_id,
    p.prediction_id,
    p.status_url,
    p.status,
    p.error,
    p.outputs AS "outputs: Json<Vec<GeneratedArtifact>>",
    p.created_at,
    p.updated_at,
    p.completed_at,
    t.status AS "task_status?: TaskStatus",
    t.error_code AS "task_error_code?",
    t.error_message AS "task_error?",
    t.completed_at AS "task_completed_at?"
FROM media.prediction AS p
-- The run's Task in core's Task runtime, whose ending the run's status follows.
LEFT JOIN tasks.task AS t ON t.id = p.task_id AND t.tenant_id = p.tenant
WHERE p.tenant = $1 AND p.task_id = $2
