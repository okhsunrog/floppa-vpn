-- Pin the closed value sets that the code models as enums (floppa_core::models), so a typo in a
-- hand-written UPDATE or an ad-hoc psql session cannot leave a row no reader recognises.
-- Values are exactly those written today; 'vless' peers were deleted in 0003.

ALTER TABLE peers
    ADD CONSTRAINT peers_sync_status_check
    CHECK (sync_status IN ('pending_add', 'active', 'pending_remove', 'removed'));

ALTER TABLE peers
    ADD CONSTRAINT peers_protocol_check
    CHECK (protocol IN ('wireguard', 'amneziawg'));

ALTER TABLE subscriptions
    ADD CONSTRAINT subscriptions_source_check
    CHECK (source IN ('trial', 'taster', 'purchase', 'admin_grant'));
