-- A page of the viewer's runs after a cursor's Task.
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
WHERE tenant = $1 AND owner = $2 AND data_labels <@ $3 AND task_id < $4
ORDER BY task_id DESC
LIMIT $5
