-- A run's row keeps what only Media knows about the run: its model, the provider's
-- prediction and status endpoint, how the provider's outcome reaches it, its stored
-- outputs and when Media recorded it. The run's owner, status, failure and times live in
-- core's Task runtime, which Media reads through the runtime's API.
ALTER TABLE media.prediction
    DROP COLUMN owner,
    DROP COLUMN data_labels,
    DROP COLUMN status,
    DROP COLUMN error,
    DROP COLUMN updated_at,
    DROP COLUMN completed_at,
    ADD COLUMN completion text CHECK (completion IN ('events', 'checks'));

-- The sweep's walk through runs past their retention, oldest first.
CREATE INDEX prediction_created ON media.prediction (created_at, task_id);

COMMENT ON TABLE media.prediction IS
    'Retention: from the configured number of days after Media recorded the run until '
    'the sweep finds its Task gone from core''s Task runtime and deletes the row; the '
    'outputs live on in Artifact under their own retention.';
