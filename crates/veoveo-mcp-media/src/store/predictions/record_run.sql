-- Record a run when its Task starts. A repeat changes nothing.
INSERT INTO media.prediction (tenant, task_id, model_id, outputs, created_at)
VALUES ($1, $2, $3, '[]', $4)
ON CONFLICT DO NOTHING
