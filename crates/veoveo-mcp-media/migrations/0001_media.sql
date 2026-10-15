-- Media's tables. A run's row keeps what only Media knows about the run: its model, the
-- provider's prediction and status endpoint, how the provider's outcome reaches it, its
-- stored outputs and when Media recorded it. The run's owner, status, failure and times
-- live in core's Task runtime, which Media reads through the runtime's API, and core's
-- usage ledger holds its costs. `tenant` holds the identity schema's deterministic UUID.

CREATE TABLE media.prediction (
    tenant uuid NOT NULL,
    task_id uuid NOT NULL,
    model_id text NOT NULL CHECK (
        octet_length(model_id) BETWEEN 1 AND 512 AND model_id ~ '^[A-Za-z0-9_.-]+(/[A-Za-z0-9_.-]+)*$'
    ),
    prediction_id text CHECK (
        octet_length(prediction_id) BETWEEN 1 AND 512 AND prediction_id !~ '[/[:cntrl:]]'
    ),
    -- The provider's status endpoint of the prediction, which a run without a callback
    -- checks for its outcome.
    status_url text CHECK (octet_length(status_url) BETWEEN 1 AND 2048),
    outputs jsonb NOT NULL CHECK (jsonb_typeof(outputs) = 'array'),
    created_at timestamptz NOT NULL,
    completion text CHECK (completion IN ('events', 'checks')),
    PRIMARY KEY (tenant, task_id)
);

COMMENT ON TABLE media.prediction IS
    'Retention: from the configured number of days after Media recorded the run until '
    'the sweep finds its Task gone from core''s Task runtime and deletes the row; the '
    'outputs live on in Artifact under their own retention.';

-- A provider names each of its predictions once per tenant.
CREATE UNIQUE INDEX prediction_provider ON media.prediction (tenant, prediction_id)
    WHERE prediction_id IS NOT NULL;
-- A run's status check finds its prediction by the Task, whose UUIDv7 is unique.
CREATE UNIQUE INDEX prediction_task ON media.prediction (task_id);
-- The sweep's walk through runs past their retention, oldest first.
CREATE INDEX prediction_created ON media.prediction (created_at, task_id);

CREATE TRIGGER notify_change AFTER INSERT OR UPDATE OR DELETE ON media.prediction
    FOR EACH ROW EXECUTE FUNCTION kernel.notify_change('task_id', 'tenant');
