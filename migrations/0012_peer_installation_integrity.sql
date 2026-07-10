-- Enforce the ownership and per-protocol uniqueness invariants used by peer creation.

-- Historical rows created through the old API may point at another user's installation. Keep the
-- peer, but detach the invalid device association before adding the composite foreign key.
UPDATE peers p
SET installation_id = NULL
WHERE p.installation_id IS NOT NULL
  AND NOT EXISTS (
      SELECT 1
      FROM app_installations ai
      WHERE ai.id = p.installation_id
        AND ai.user_id = p.user_id
  );

-- If duplicate active rows already exist, preserve the active/oldest peer and queue the rest for
-- daemon removal. pending_remove rows are deliberately outside the partial unique index.
WITH ranked AS (
    SELECT id,
           ROW_NUMBER() OVER (
               PARTITION BY installation_id, protocol
               ORDER BY CASE sync_status WHEN 'active' THEN 0 ELSE 1 END,
                        created_at,
                        id
           ) AS position
    FROM peers
    WHERE installation_id IS NOT NULL
      AND sync_status NOT IN ('removed', 'pending_remove')
)
UPDATE peers p
SET sync_status = 'pending_remove'
FROM ranked r
WHERE p.id = r.id
  AND r.position > 1;

CREATE UNIQUE INDEX IF NOT EXISTS peers_installation_protocol_active
    ON peers (installation_id, protocol)
    WHERE installation_id IS NOT NULL
      AND sync_status NOT IN ('removed', 'pending_remove');

-- A plain installation_id FK cannot ensure that peer.user_id and installation.user_id agree.
ALTER TABLE app_installations
    ADD CONSTRAINT app_installations_id_user_id_unique UNIQUE (id, user_id);

ALTER TABLE peers
    ADD CONSTRAINT peers_installation_owner_fk
    FOREIGN KEY (installation_id, user_id)
    REFERENCES app_installations (id, user_id)
    DEFERRABLE INITIALLY DEFERRED;
