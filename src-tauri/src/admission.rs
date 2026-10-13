//! The admission gate (spec §3.4): every mutating entry point holds a ticket for its
//! whole duration, and a drain waits for the tickets — not for a "busy" flag, which a
//! request admitted a moment before the drain may not have raised yet.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use tokio::sync::Notify;

#[derive(Default)]
struct Inner {
    draining: bool,
    in_flight: usize,
}

#[derive(Default)]
pub struct Admission {
    inner: Mutex<Inner>,
    released: Notify,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Draining;

/// Held for a mutating request's whole duration; dropping it lets a drain proceed.
pub struct Ticket {
    gate: Arc<Admission>,
}

impl Drop for Ticket {
    fn drop(&mut self) {
        let mut inner = self.gate.inner.lock().expect("admission");
        inner.in_flight -= 1;
        drop(inner);
        self.gate.released.notify_waiters();
    }
}

impl Admission {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check and count under one lock: no request can be admitted after a drain began
    /// without being counted by it.
    pub fn enter(self: &Arc<Self>) -> Result<Ticket, Draining> {
        let mut inner = self.inner.lock().expect("admission");
        if inner.draining {
            return Err(Draining);
        }
        inner.in_flight += 1;
        Ok(Ticket { gate: self.clone() })
    }

    pub fn is_draining(&self) -> bool {
        self.inner.lock().expect("admission").draining
    }

    pub fn in_flight(&self) -> usize {
        self.inner.lock().expect("admission").in_flight
    }

    /// Stop admitting, then wait for every admitted request. `false` when `cap` ran out.
    pub async fn drain(&self, cap: Duration) -> bool {
        self.inner.lock().expect("admission").draining = true;
        let wait = async {
            loop {
                let released = self.released.notified();
                tokio::pin!(released);
                // Register before checking, so a release between the check and the await
                // is not missed.
                released.as_mut().enable();
                if self.in_flight() == 0 {
                    return;
                }
                released.await;
            }
        };
        tokio::time::timeout(cap, wait).await.is_ok()
    }
}

/// Every non-GET/HEAD/OPTIONS request holds a ticket; while draining it is refused.
pub async fn gate(State(state): State<crate::server::AppState>, req: Request, next: Next) -> Response {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return next.run(req).await;
    }
    match state.admission.enter() {
        Ok(ticket) => {
            let res = next.run(req).await;
            drop(ticket);
            res
        }
        Err(Draining) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "code": "updating",
                "error": "Ledger POS is closing for an update; retry against the new version in a moment",
            })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn a_ticket_taken_before_the_drain_holds_it_open() {
        let a = Arc::new(Admission::new());
        // The race: admitted, but has not reached the provider yet — nothing is "busy".
        let ticket = a.enter().expect("admitted");
        let drain = tokio::spawn({
            let a = a.clone();
            async move { a.drain(Duration::from_secs(5)).await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!drain.is_finished(), "must wait for the admitted request");
        assert!(a.enter().is_err(), "nothing is admitted once draining");
        drop(ticket);
        assert!(drain.await.unwrap(), "drained, not capped");
    }

    #[tokio::test]
    async fn the_cap_ends_a_drain_that_never_empties() {
        let a = Arc::new(Admission::new());
        let _stuck = a.enter().unwrap();
        assert!(!a.drain(Duration::from_millis(50)).await);
    }

    #[tokio::test]
    async fn an_empty_gate_drains_at_once() {
        let a = Arc::new(Admission::new());
        assert!(a.drain(Duration::from_millis(10)).await);
        assert!(a.is_draining());
    }
}
