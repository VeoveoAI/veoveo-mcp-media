-- The provider accepted the run, under its prediction and status endpoint when it named
-- them, and with how its outcome reaches the run once that is known. A later call
-- without them keeps what an earlier one recorded.
UPDATE media.prediction
SET prediction_id = COALESCE($3, prediction_id),
    status_url = COALESCE($4, status_url),
    completion = COALESCE($5, completion)
WHERE tenant = $1 AND task_id = $2
