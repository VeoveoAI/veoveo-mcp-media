-- One run by its Task.
SELECT
    tenant AS "tenant: StoredTenant",
    task_id,
    owner AS "owner: StoredPrincipal",
    data_labels,
    model_id,
    prediction_id,
    status_url,
    status,
    error,
    outputs AS "outputs: Json<Vec<GeneratedArtifact>>",
    created_at,
    updated_at,
    completed_at
FROM media.prediction
WHERE tenant = $1 AND task_id = $2
