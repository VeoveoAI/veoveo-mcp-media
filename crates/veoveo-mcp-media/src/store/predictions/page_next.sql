-- A page of the viewer's runs after a cursor's Task. The page is chosen first, and only
-- its runs read their Tasks.
WITH page AS MATERIALIZED (
    SELECT
        tenant, task_id, owner, data_labels, model_id, prediction_id, status_url, status,
        error, outputs, created_at, updated_at, completed_at, completion
    FROM media.prediction
    WHERE tenant = $1 AND owner = $2 AND data_labels <@ $3 AND task_id < $4
    ORDER BY task_id DESC
    LIMIT $5
)
SELECT
    page.tenant AS "tenant!: StoredTenant",
    page.task_id AS "task_id!",
    page.owner AS "owner!: StoredPrincipal",
    page.data_labels AS "data_labels!",
    page.model_id AS "model_id!",
    page.prediction_id AS "prediction_id?",
    page.status_url AS "status_url?",
    page.status AS "status!",
    page.error AS "error?",
    page.outputs AS "outputs!: Json<Vec<GeneratedArtifact>>",
    page.created_at AS "created_at!",
    page.updated_at AS "updated_at!",
    page.completed_at AS "completed_at?",
    page.completion AS "completion?",
    t.status AS "task_status?: TaskStatus",
    t.error_code AS "task_error_code?",
    t.error_message AS "task_error?",
    t.completed_at AS "task_completed_at?"
FROM page
-- Each run's Task in core's Task runtime, whose ending the run's status follows.
LEFT JOIN tasks.task AS t ON t.id = page.task_id AND t.tenant_id = page.tenant
ORDER BY page.task_id DESC
