-- Private networks: operator networks a plan may grant access to.
--
-- By default a client reaches the public internet and nothing else (floppa-daemon's
-- `inet floppa` table). A private network is an explicit exception, granted per plan the same
-- way exit regions are (`plan_regions`): the daemon lets a client in when the client's current,
-- active subscription is on a plan linked to the network, and the server hands the network's
-- CIDRs to that client as routes.
--
-- Two invariants keep a grant from reaching anyone by accident. Both are enforced here, so no
-- writer (admin API, bot, psql) can get around them:
--   1. A network can be linked only to a plan that is not public (`plans.is_public = false`),
--      and such a plan cannot be made public. The bot never sells it.
--   2. A plan with a network can be the current subscription only of an administrator
--      (`users.is_admin`), and an administrator on such a plan cannot lose the flag.
--
-- Rows here are deployment data, not schema: an operator inserts them, e.g.
--   INSERT INTO private_networks (id, display_name, cidrs) VALUES ('home', 'Home', '{10.66.66.0/24}');
--   INSERT INTO plan_private_networks SELECT id, 'home' FROM plans WHERE name = 'admin';

CREATE FUNCTION cidrs_are_ipv4(nets CIDR[]) RETURNS BOOLEAN
LANGUAGE sql IMMUTABLE AS $$
    SELECT COALESCE(bool_and(family(n) = 4), true) FROM unnest(nets) AS n
$$;

CREATE TABLE private_networks (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    -- IPv4 only: the daemon's rules and the client's routes are IPv4.
    cidrs CIDR[] NOT NULL CHECK (cardinality(cidrs) > 0 AND cidrs_are_ipv4(cidrs)),
    is_active BOOLEAN NOT NULL DEFAULT true
);

CREATE TABLE plan_private_networks (
    plan_id INT NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
    network_id TEXT NOT NULL REFERENCES private_networks(id) ON DELETE CASCADE,
    PRIMARY KEY (plan_id, network_id)
);

CREATE INDEX idx_plan_private_networks_network ON plan_private_networks(network_id);

-- -----------------------------------------------------------------------------
-- Invariant 1: only non-public plans carry networks.
-- -----------------------------------------------------------------------------

CREATE FUNCTION check_plan_private_network_link() RETURNS TRIGGER AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM plans WHERE id = NEW.plan_id AND is_public) THEN
        RAISE EXCEPTION 'plan % is public; a private network can be linked only to a non-public plan', NEW.plan_id
            USING ERRCODE = 'check_violation', CONSTRAINT = 'private_network_plan_not_public';
    END IF;
    -- Linking a network to a plan someone is already on grants it to them now.
    IF EXISTS (
        SELECT 1 FROM subscriptions s JOIN users u ON u.id = s.user_id
        WHERE s.plan_id = NEW.plan_id AND s.is_current AND NOT u.is_admin
    ) THEN
        RAISE EXCEPTION 'plan % is the current subscription of a non-administrator', NEW.plan_id
            USING ERRCODE = 'check_violation', CONSTRAINT = 'private_network_admin_only';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER plan_private_network_link_check
    BEFORE INSERT OR UPDATE ON plan_private_networks
    FOR EACH ROW EXECUTE FUNCTION check_plan_private_network_link();

CREATE FUNCTION check_plan_publicity() RETURNS TRIGGER AS $$
BEGIN
    IF NEW.is_public AND NOT OLD.is_public
       AND EXISTS (SELECT 1 FROM plan_private_networks WHERE plan_id = NEW.id) THEN
        RAISE EXCEPTION 'plan % grants a private network and cannot be made public', NEW.id
            USING ERRCODE = 'check_violation', CONSTRAINT = 'private_network_plan_not_public';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER plan_publicity_check
    BEFORE UPDATE OF is_public ON plans
    FOR EACH ROW EXECUTE FUNCTION check_plan_publicity();

-- -----------------------------------------------------------------------------
-- Invariant 2: only administrators are on such a plan.
-- -----------------------------------------------------------------------------

CREATE FUNCTION check_subscription_private_networks() RETURNS TRIGGER AS $$
BEGIN
    IF NEW.is_current
       AND EXISTS (SELECT 1 FROM plan_private_networks WHERE plan_id = NEW.plan_id)
       AND NOT EXISTS (SELECT 1 FROM users WHERE id = NEW.user_id AND is_admin) THEN
        RAISE EXCEPTION 'plan % grants a private network; only an administrator can be subscribed to it', NEW.plan_id
            USING ERRCODE = 'check_violation', CONSTRAINT = 'private_network_admin_only';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER subscription_private_networks_check
    BEFORE INSERT OR UPDATE OF plan_id, is_current, user_id ON subscriptions
    FOR EACH ROW EXECUTE FUNCTION check_subscription_private_networks();

CREATE FUNCTION check_admin_revocation() RETURNS TRIGGER AS $$
BEGIN
    IF OLD.is_admin AND NOT NEW.is_admin AND EXISTS (
        SELECT 1 FROM subscriptions s
        JOIN plan_private_networks ppn ON ppn.plan_id = s.plan_id
        WHERE s.user_id = NEW.id AND s.is_current
    ) THEN
        RAISE EXCEPTION 'user % is on a plan that grants a private network; move them off it first', NEW.id
            USING ERRCODE = 'check_violation', CONSTRAINT = 'private_network_admin_only';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER admin_revocation_check
    BEFORE UPDATE OF is_admin ON users
    FOR EACH ROW EXECUTE FUNCTION check_admin_revocation();

-- -----------------------------------------------------------------------------
-- The daemon rebuilds its grants on any change here. Subscription changes already arrive as
-- `subscription_changed`.
-- -----------------------------------------------------------------------------

CREATE FUNCTION notify_private_networks_changed() RETURNS TRIGGER AS $$
BEGIN
    PERFORM pg_notify('private_networks_changed', '');
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER private_networks_changed
    AFTER INSERT OR UPDATE OR DELETE ON private_networks
    FOR EACH STATEMENT EXECUTE FUNCTION notify_private_networks_changed();

CREATE TRIGGER plan_private_networks_changed
    AFTER INSERT OR UPDATE OR DELETE ON plan_private_networks
    FOR EACH STATEMENT EXECUTE FUNCTION notify_private_networks_changed();
