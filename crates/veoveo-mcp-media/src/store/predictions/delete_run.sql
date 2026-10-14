-- Delete one run whose Task core's Task runtime no longer holds.
DELETE FROM media.prediction
WHERE tenant = $1 AND task_id = $2
