-- Drop stale VLESS traffic columns from users table.
-- Traffic is now tracked via VictoriaMetrics (vless_tx_bytes_total / vless_rx_bytes_total).
ALTER TABLE users DROP COLUMN vless_tx_bytes, DROP COLUMN vless_rx_bytes;
