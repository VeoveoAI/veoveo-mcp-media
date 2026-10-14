-- The sweep's queue of runs still open in Media, which it settles once their Task in
-- core's Task runtime has ended.
CREATE INDEX prediction_open ON media.prediction (task_id) WHERE completed_at IS NULL;
