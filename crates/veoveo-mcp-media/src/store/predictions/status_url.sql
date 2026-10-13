-- veoveo: all tenants (a status check names a run by its Task, whose identity is unique across tenants)
-- The status endpoint a run's provider named for its prediction.
SELECT status_url
FROM media.prediction
WHERE task_id = $1
