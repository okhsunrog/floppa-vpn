# Local frontend development

This runs the **real** `floppa-face`, `floppa-server`, authentication, migrations and
PostgreSQL. There is no alternate demo application, fake login or API interceptor.
No deployed service or existing VPN configuration is used.

## Start

Prerequisites: Linux, Rust, Vite+ (`vp`), `uv`, `just`, and Docker Compose. The supervisor
prefers rootless Podman when installed, otherwise Docker. With Podman, start its
user socket once (`systemctl --user start podman.socket`); with Docker, start its
daemon. Set `FLOPPA_DEV_CONTAINER=docker` or `podman` to choose explicitly. Keep
using that engine for reset/down because their volumes are separate. From the checkout:

```sh
just dev
```

Open **http://127.0.0.1:5173/login**. Sign in with either account:

| Login | Password | Role |
| --- | --- | --- |
| `dev-admin` | `floppa-local-2026` | Administrator |
| `dev-user` | `floppa-local-2026` | Regular user |

The first start downloads PostgreSQL, builds the frontend/server, applies the
embedded migrations and seeds an empty database. Later starts preserve edits.
Vite serves the actual source with HMR. Restart `just dev` after Rust changes.
API and Vite listen on loopback. PostgreSQL is exposed on `127.0.0.1:54329`.
The database container uses the host network and PostgreSQL explicitly listens
only on loopback. This avoids bridge/firewall/TUN dependencies on a VPN development
host. It does not change system routing or firewall settings.

`Ctrl+C` stops both development servers; the database container and volume stay.
`just dev-down` stops the database while retaining data. `just dev-reset` deletes
**only this checkout/instance's Compose database volume**, reapplies migrations and
seeds fresh data. Stop the dev process first; a lock rejects concurrent starts or
resets. No command accepts a production database URL.

The seed includes 38 users, active/permanent/expired subscriptions, tariff region
access, devices, and configs pending application. Newly created configs remain
pending because no daemon runs. Traffic metrics truthfully display unavailable.
Telegram login/notifications/payments and actual network connections are not
available in this environment; administrative database operations are real.

Generated local encryption keys live in `.dev/local/secrets.toml` with permissions
0600. They are never copied from the host or production, logged, or committed.
The Compose password and test-account passwords above are intentionally public,
local-only credentials. Never expose this environment to the internet.

## Multiple real frontend variants

Run the API once in the main checkout:

```sh
just dev-api
```

In each frontend worktree, install dependencies and start Vite on a separate port:

```sh
vp install --frozen-lockfile
just dev-frontend 5181
# Other worktrees: just dev-frontend 5182, just dev-frontend 5183
```

These serve each worktree's real application, sharing the same local API/database.
An edit in one variant is visible in the others after refreshing. Each origin has
its own login storage. A worktree needs no database or server build for this mode.

Ports can be overridden before `just dev` using `FLOPPA_DEV_WEB_PORT`,
`FLOPPA_DEV_API_PORT`, and `FLOPPA_DEV_DB_PORT`. A second complete stack also needs a
unique `FLOPPA_DEV_INSTANCE` (lowercase letters, digits and hyphens). Use the same
variables for reset/down; changing the database port of an existing instance is
rejected. The Compose project includes a hash of the checkout path to isolate
volumes across worktrees. The API URL cannot silently inherit a remote value in
these recipes. Plain `vp dev` retains the existing `FLOPPA_DEV_API` proxy workflow.

## Browser regression tests

```sh
just frontend-browser-install
just frontend-e2e
```

Playwright starts a separate **real** stack (`e2e` instance, web 5177, API 3117,
PostgreSQL 54339) and logs in normally. It serves the built frontend with `vp preview`
and exercises plan CRUD, subscription filters and permissions.
It does not intercept API responses. Test entities use unique names and are
removed after each run. The regular dev database is untouched. To remove the test
container after the suite:

```sh
FLOPPA_DEV_INSTANCE=e2e FLOPPA_DEV_DB_PORT=54339 just dev-down
```

Failure screenshots/traces are under `floppa-face/test-results/`; the HTML report
is under `floppa-face/playwright-report/`. CI runs the same suite.

## Server options

`floppa-server --api-only` skips Telegram configuration, dispatcher and notification
jobs and hides Telegram login in public configuration. It keeps ordinary password
login, authorization and all database-backed routes. `--migrate` applies embedded
migrations and exits. Default server behaviour is unchanged.

`FLOPPA_LISTEN` overrides the API address (default `0.0.0.0:3000`);
`FLOPPA_METRICS_LISTEN` overrides the metrics listener (default `127.0.0.1:9102`).
The dev supervisor supplies explicit local config/secrets paths and loopback
listeners, and terminates its child process groups on exit.

## Persistent preview worktrees (Linux)

Run `just dev-background` to keep the real frontend/API running as a user systemd
service after closing a terminal or finishing a browser review. It runs the same
`just dev` preparation, migrations and seed. No root access or boot autostart.
Use distinct ports and instances in each worktree, for example:

```sh
FLOPPA_DEV_INSTANCE=tabs FLOPPA_DEV_LABEL=Tabs \
FLOPPA_DEV_WEB_PORT=5182 FLOPPA_DEV_API_PORT=3102 FLOPPA_DEV_DB_PORT=54332 \
just dev-background

# In the other worktree:
FLOPPA_DEV_INSTANCE=sidebar FLOPPA_DEV_LABEL=Sidebar \
FLOPPA_DEV_WEB_PORT=5183 FLOPPA_DEV_API_PORT=3103 FLOPPA_DEV_DB_PORT=54333 \
just dev-background
```

`FLOPPA_DEV_INSTANCE=tabs just dev-status` shows status and
`FLOPPA_DEV_INSTANCE=tabs just dev-stop` stops its API/frontend. Run these in the
same worktree used to start the preview. The printed service name also works
with `journalctl --user -u NAME -f`. PostgreSQL data persists after stopping;
`FLOPPA_DEV_INSTANCE=tabs just dev-down` stops its database container.

Fresh databases contain first payments, repeat payments, credit-funded changes,
manual grants and trials. Existing data is preserved. Analytics query the real
local database. Traffic metrics are unavailable unless VictoriaMetrics is supplied;
the UI shows unavailable data rather than fabricated activity.
