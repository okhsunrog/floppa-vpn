-- The Moscow gateway can also be selected as a direct exit. Access remains
-- plan-specific and is granted only to the dedicated administrator plan.

INSERT INTO regions (id, display_name, sort_order, supports_vless)
VALUES ('moscow', 'Moscow', 20, true)
ON CONFLICT (id) DO UPDATE SET
    display_name = EXCLUDED.display_name,
    sort_order = EXCLUDED.sort_order,
    supports_vless = EXCLUDED.supports_vless;

UPDATE regions SET sort_order = 30 WHERE id = 'singapore';

INSERT INTO plan_regions (plan_id, region_id)
SELECT id, 'moscow' FROM plans WHERE name = 'admin'
ON CONFLICT DO NOTHING;
