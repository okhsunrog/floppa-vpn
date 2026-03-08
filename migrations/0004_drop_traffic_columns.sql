-- Drop traffic columns that are now sourced from VictoriaMetrics
ALTER TABLE peers DROP COLUMN tx_bytes, DROP COLUMN rx_bytes, DROP COLUMN traffic_used_bytes;

-- Remove traffic limit from plans (not used)
ALTER TABLE plans DROP COLUMN default_traffic_limit_bytes;
