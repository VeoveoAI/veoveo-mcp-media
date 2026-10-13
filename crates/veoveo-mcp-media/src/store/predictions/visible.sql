-- One prediction by the provider's identity, when the viewer owns it and holds its labels.
-- A prediction the viewer may not see reads as missing.
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
WHERE tenant = $1 AND prediction_id = $2 AND owner = $3 AND data_labels <@ $4
