-- How the provider's outcome reaches a run: its signed webhook, or Media's checks of the
-- prediction's status endpoint. Recorded once the run reaches the provider.
ALTER TABLE media.prediction ADD COLUMN completion text CHECK (completion IN ('events', 'checks'));
