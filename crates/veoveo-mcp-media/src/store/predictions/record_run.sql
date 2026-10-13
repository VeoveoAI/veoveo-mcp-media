-- Record a run of the owner's when its Task starts. A repeat changes nothing.
INSERT INTO media.prediction (
    tenant, task_id, owner, data_labels, model_id, status, outputs, created_at, updated_at
)
VALUES ($1, $2, $3, $4, $5, 'queued', '[]', $6, $6)
ON CONFLICT (tenant, task_id) DO NOTHING
