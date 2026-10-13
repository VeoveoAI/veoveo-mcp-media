-- Media's tables. A prediction row tracks one `run` Task for the resources Media serves;
-- core's Task runtime holds the Task, its steps and its provider operations, and core's
-- usage ledger holds its costs. `tenant` and `owner` hold the identity schema's
-- deterministic UUIDs, and the owner's data labels travel as their text keys.

CREATE TABLE media.prediction (
    tenant uuid NOT NULL,
    task_id uuid NOT NULL,
    owner uuid NOT NULL,
    data_labels text[] NOT NULL CHECK (
        cardinality(data_labels) <= 64 AND array_position(data_labels, NULL) IS NULL
    ),
    model_id text NOT NULL CHECK (
        octet_length(model_id) BETWEEN 1 AND 512 AND model_id ~ '^[A-Za-z0-9_.-]+(/[A-Za-z0-9_.-]+)*$'
    ),
    prediction_id text CHECK (
        octet_length(prediction_id) BETWEEN 1 AND 512 AND prediction_id !~ '[/[:cntrl:]]'
    ),
    -- The provider's status endpoint of the prediction, which a run without a callback
    -- checks for its outcome.
    status_url text CHECK (octet_length(status_url) BETWEEN 1 AND 2048),
    status text NOT NULL CHECK (
        status IN ('queued', 'processing', 'storing', 'succeeded', 'failed', 'cancelled', 'unresolved')
    ),
    error text CHECK (octet_length(error) BETWEEN 1 AND 2048),
    outputs jsonb NOT NULL CHECK (jsonb_typeof(outputs) = 'array'),
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    completed_at timestamptz,
    PRIMARY KEY (tenant, task_id),
    CHECK ((completed_at IS NOT NULL) = (status IN ('succeeded', 'failed', 'cancelled', 'unresolved')))
);

-- A provider names each of its predictions once per tenant.
CREATE UNIQUE INDEX prediction_provider ON media.prediction (tenant, prediction_id)
    WHERE prediction_id IS NOT NULL;
-- A run's status check finds its prediction by the Task, whose UUIDv7 is unique.
CREATE UNIQUE INDEX prediction_task ON media.prediction (task_id);
-- The owner's listing, newest first by the Task's UUIDv7.
CREATE INDEX prediction_owner_page ON media.prediction (tenant, owner, task_id);
-- The sweep's queue across tenants.
CREATE INDEX prediction_completed ON media.prediction (completed_at)
    WHERE completed_at IS NOT NULL;

COMMENT ON TABLE media.prediction IS
    'Retention: until the configured number of days after the run ended, when the sweep '
    'deletes the row; the outputs live on in Artifact under their own retention.';

CREATE TRIGGER notify_change AFTER INSERT OR UPDATE OR DELETE ON media.prediction
    FOR EACH ROW EXECUTE FUNCTION kernel.notify_change('task_id', 'tenant');
