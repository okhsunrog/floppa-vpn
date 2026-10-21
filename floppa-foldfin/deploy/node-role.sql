-- Run as database administrator, after migrations. Set the password out of band.
-- Use this role for Serve, not Register (deployment registration needs separate credentials).
CREATE ROLE foldfin_node LOGIN;
GRANT CONNECT ON DATABASE floppa TO foldfin_node;
GRANT USAGE ON SCHEMA public TO foldfin_node;
GRANT SELECT ON foldfin_nodes, foldfin_devices, app_installations, sessions,
    current_subscriptions, plans, plan_regions, regions, plan_private_networks,
    private_networks TO foldfin_node;
GRANT UPDATE (applied_revision) ON foldfin_devices TO foldfin_node;
GRANT UPDATE (last_sync_at) ON foldfin_nodes TO foldfin_node;
-- The role can observe all regional grants. Use private DB networking + verified TLS;
-- do not expose PostgreSQL to VPN clients. Per-region RLS is not implemented.
