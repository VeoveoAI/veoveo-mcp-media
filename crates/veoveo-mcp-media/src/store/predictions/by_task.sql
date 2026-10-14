-- One run by its Task.
SELECT
    tenant AS "tenant: StoredTenant",
    task_id,
    model_id,
    prediction_id,
    status_url,
    completion,
    outputs AS "outputs: Json<Vec<GeneratedArtifact>>",
    created_at
FROM media.prediction
WHERE tenant = $1 AND task_id = $2
