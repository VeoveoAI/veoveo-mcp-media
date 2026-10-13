-- The provider finished the run, and Media stores its outputs.
UPDATE media.prediction
SET status = 'storing', prediction_id = COALESCE(prediction_id, $3), updated_at = $4
WHERE tenant = $1 AND task_id = $2 AND completed_at IS NULL
