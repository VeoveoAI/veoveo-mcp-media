-- End the run with its final status, its error or its stored outputs. An ended run keeps
-- its first ending.
UPDATE media.prediction
SET status = $3,
    error = $4,
    outputs = $5,
    prediction_id = COALESCE(prediction_id, $6),
    completed_at = $7,
    updated_at = $7
WHERE tenant = $1 AND task_id = $2 AND completed_at IS NULL
