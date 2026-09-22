-- The European exit is physically located in Frankfurt. Keep the stable ID so
-- existing device selections and plan grants need no migration.

UPDATE regions
SET display_name = 'Frankfurt'
WHERE id = 'europe';
