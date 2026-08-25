//! Prometheus metrics for floppa-server.
//!
//! The daemon and floppa-vless export traffic counters (`wg_*`, `vless_*`); this process
//! exports what only it can see — the HTTP API and the bot. Everything is recorded through
//! the [`metrics`] facade and served by the exporter installed in [`install_exporter`], which
//! VictoriaMetrics scrapes at `127.0.0.1:9102` (cloud-forge `promscrape.yml`).
//!
//! Label cardinality is kept bounded on purpose: routes are the *matched* axum path template
//! (`/api/me/peers/{id}`), never the raw URI, and per-user labels are avoided — the traffic
//! exporters already carry `user_id`.

use std::{net::Ipv4Addr, time::Instant};

use axum::{
    extract::{MatchedPath, Request},
    middleware::Next,
    response::Response,
};
use metrics::{
    Unit, counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram,
};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};

/// Where the exporter listens. Loopback only: the scrape target is the local VictoriaMetrics.
pub const LISTEN: (Ipv4Addr, u16) = (Ipv4Addr::LOCALHOST, 9102);

const HTTP_REQUESTS: &str = "http_requests_total";
const HTTP_DURATION: &str = "http_request_duration_seconds";
const HTTP_IN_FLIGHT: &str = "http_requests_in_flight";
const RATE_LIMITED: &str = "auth_rate_limited_total";
const UPGRADE_REQUIRED: &str = "client_upgrade_required_total";
const TOKENS_REFRESHED: &str = "auth_tokens_refreshed_total";
const TOKENS_REJECTED: &str = "auth_tokens_rejected_total";
const SERVER_ERRORS: &str = "api_server_errors_total";
const VM_QUERY_FAILURES: &str = "vm_query_failures_total";
const BOT_UPDATES: &str = "bot_updates_total";
const BOT_PAYMENTS: &str = "bot_payments_total";

/// Latency buckets for the request histogram: an API answering from a local Postgres sits
/// in the low milliseconds, Telegram/VictoriaMetrics round-trips reach into seconds.
const DURATION_BUCKETS: [f64; 12] = [
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0,
];

/// Start the exporter and register HELP/TYPE text for every series this process emits.
pub fn install_exporter() -> anyhow::Result<()> {
    PrometheusBuilder::new()
        .with_http_listener(LISTEN)
        .set_buckets_for_metric(Matcher::Full(HTTP_DURATION.into()), &DURATION_BUCKETS)?
        .install()?;

    describe_counter!(
        HTTP_REQUESTS,
        Unit::Count,
        "API requests by method, matched route template and response status"
    );
    describe_histogram!(
        HTTP_DURATION,
        Unit::Seconds,
        "API request latency by method and matched route template"
    );
    describe_gauge!(
        HTTP_IN_FLIGHT,
        Unit::Count,
        "API requests currently being handled"
    );
    describe_counter!(
        RATE_LIMITED,
        Unit::Count,
        "Auth attempts rejected by the rate limiter, by scope"
    );
    describe_counter!(
        UPGRADE_REQUIRED,
        Unit::Count,
        "Requests refused with 426 because the client is older than min_client_version"
    );
    describe_counter!(
        TOKENS_REFRESHED,
        Unit::Count,
        "JWTs re-issued by the sliding-session middleware"
    );
    describe_counter!(
        TOKENS_REJECTED,
        Unit::Count,
        "Correctly signed JWTs refused by the user/session check, by reason"
    );
    describe_counter!(
        SERVER_ERRORS,
        Unit::Count,
        "5xx responses by error code (database_error, upstream_error, ...)"
    );
    describe_counter!(
        VM_QUERY_FAILURES,
        Unit::Count,
        "VictoriaMetrics traffic queries that failed, by query"
    );
    describe_counter!(
        BOT_UPDATES,
        Unit::Count,
        "Telegram updates that reached a bot handler, by outcome (ok/error)"
    );
    describe_counter!(
        BOT_PAYMENTS,
        Unit::Count,
        "Telegram Stars payments by outcome (fulfilled/duplicate/unfulfilled)"
    );
    Ok(())
}

/// Outermost API layer: one counter and one histogram sample per request, keyed by the route
/// template axum matched (so `/me/peers/42` and `/me/peers/43` are the same series).
pub async fn http_metrics(request: Request, next: Next) -> Response {
    let method = request.method().as_str().to_owned();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_owned(), |p| p.as_str().to_owned());

    let in_flight = gauge!(HTTP_IN_FLIGHT);
    in_flight.increment(1.0);
    let started = Instant::now();
    let response = next.run(request).await;
    in_flight.decrement(1.0);

    histogram!(HTTP_DURATION, "method" => method.clone(), "route" => route.clone())
        .record(started.elapsed().as_secs_f64());
    counter!(
        HTTP_REQUESTS,
        "method" => method,
        "route" => route,
        "status" => response.status().as_u16().to_string()
    )
    .increment(1);
    response
}

pub fn rate_limited(scope: &'static str) {
    counter!(RATE_LIMITED, "scope" => scope).increment(1);
}

pub fn upgrade_required() {
    counter!(UPGRADE_REQUIRED).increment(1);
}

pub fn token_refreshed() {
    counter!(TOKENS_REFRESHED).increment(1);
}

/// A correctly signed token was refused by the session/user check, by reason.
pub fn token_rejected(reason: &'static str) {
    counter!(TOKENS_REJECTED, "reason" => reason).increment(1);
}

pub fn server_error(error: &'static str) {
    counter!(SERVER_ERRORS, "error" => error).increment(1);
}

pub fn vm_query_failed(query: &'static str) {
    counter!(VM_QUERY_FAILURES, "query" => query).increment(1);
}

/// How a Telegram update left the handler tree.
#[derive(Debug, Clone, Copy)]
pub enum BotOutcome {
    Ok,
    Error,
}

pub fn bot_update(outcome: BotOutcome) {
    let outcome = match outcome {
        BotOutcome::Ok => "ok",
        BotOutcome::Error => "error",
    };
    counter!(BOT_UPDATES, "outcome" => outcome).increment(1);
}

/// What became of a successful Telegram Stars charge.
#[derive(Debug, Clone, Copy)]
pub enum PaymentOutcome {
    /// Subscription granted.
    Fulfilled,
    /// Telegram re-delivered an update we had already processed.
    Duplicate,
    /// Charged but not granted; recorded as a failed payment and admins were alerted.
    Unfulfilled,
}

pub fn bot_payment(outcome: PaymentOutcome) {
    let outcome = match outcome {
        PaymentOutcome::Fulfilled => "fulfilled",
        PaymentOutcome::Duplicate => "duplicate",
        PaymentOutcome::Unfulfilled => "unfulfilled",
    };
    counter!(BOT_PAYMENTS, "outcome" => outcome).increment(1);
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{Router, body::Body, http::StatusCode, routing::get};
    use metrics::{
        Counter, CounterFn, Gauge, Histogram, Key, KeyName, Metadata, Recorder, SharedString, Unit,
    };
    use tower::ServiceExt;

    use super::*;

    /// Records every counter increment as (key, delta) so a test can assert on labels.
    #[derive(Default)]
    struct Capture(Arc<Mutex<Vec<(Key, u64)>>>);

    struct Cell(Key, Arc<Mutex<Vec<(Key, u64)>>>);
    impl CounterFn for Cell {
        fn increment(&self, value: u64) {
            self.1.lock().unwrap().push((self.0.clone(), value));
        }
        fn absolute(&self, _: u64) {}
    }

    impl Recorder for Capture {
        fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
        fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
        fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
        fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
            Counter::from_arc(Arc::new(Cell(key.clone(), Arc::clone(&self.0))))
        }
        fn register_gauge(&self, _: &Key, _: &Metadata<'_>) -> Gauge {
            Gauge::noop()
        }
        fn register_histogram(&self, _: &Key, _: &Metadata<'_>) -> Histogram {
            Histogram::noop()
        }
    }

    #[test]
    fn requests_are_counted_by_route_template_and_status() {
        let capture = Capture::default();
        let app = Router::new()
            .route("/me/peers/{id}", get(|| async { StatusCode::NO_CONTENT }))
            .layer(axum::middleware::from_fn(http_metrics));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        metrics::with_local_recorder(&capture, || {
            rt.block_on(async {
                for path in ["/me/peers/42", "/me/peers/43", "/nope"] {
                    let req = axum::http::Request::get(path).body(Body::empty()).unwrap();
                    app.clone().oneshot(req).await.unwrap();
                }
            });
        });

        let seen = capture.0.lock().unwrap();
        let labels = |key: &Key| -> Vec<(String, String)> {
            key.labels()
                .map(|l| (l.key().to_owned(), l.value().to_owned()))
                .collect()
        };
        let requests: Vec<_> = seen
            .iter()
            .filter(|(k, _)| k.name() == HTTP_REQUESTS)
            .map(|(k, _)| labels(k))
            .collect();
        // Both ids collapse into one series; the raw path never appears as a label.
        assert_eq!(
            requests
                .iter()
                .filter(|l| l.contains(&("route".into(), "/me/peers/{id}".into()))
                    && l.contains(&("status".into(), "204".into())))
                .count(),
            2
        );
        assert!(
            requests
                .iter()
                .flatten()
                .all(|(_, v)| !v.contains("/42") && !v.contains("/43"))
        );
        // A path axum did not match is still counted, under a fixed bucket.
        assert_eq!(
            requests
                .iter()
                .filter(|l| l.contains(&("route".into(), "unmatched".into()))
                    && l.contains(&("status".into(), "404".into())))
                .count(),
            1
        );
    }
}
