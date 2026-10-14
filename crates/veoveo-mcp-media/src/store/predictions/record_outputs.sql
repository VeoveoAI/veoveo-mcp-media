-- The run's outputs, stored in Artifact, under the prediction that made them.
UPDATE media.prediction
SET outputs = $3, prediction_id = COALESCE(prediction_id, $4)
WHERE tenant = $1 AND task_id = $2
