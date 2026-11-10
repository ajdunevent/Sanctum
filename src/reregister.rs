//! Registers the device again when Home Assistant no longer has its
//! `mobile_app` registration. Attempts run only while the WebSocket is
//! connected, and only failures the server could have answered are counted.

use std::cell::Cell;
use std::time::Duration;

use gtk::glib;

use crate::ha::{self, ConnectionState};

/// The wait before each attempt. After the last attempt fails, the user is
/// told.
const DELAYS: [Duration; 5] = [
    Duration::ZERO,
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(120),
    Duration::from_secs(300),
];
/// After a failure that is not counted, so a failing connection does not
/// cause a tight loop.
const UNCOUNTED_DELAY: Duration = Duration::from_secs(30);
const CONNECTED_POLL: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct Reregistration {
    running: Cell<bool>,
    gave_up: Cell<bool>,
    /// Bumped by `cancel`; a loop from an older generation stops.
    generation: Cell<u64>,
}

impl Reregistration {
    pub fn gave_up(&self) -> bool {
        self.gave_up.get()
    }

    /// The generation for `run`, or `None` when a loop is already running or
    /// the user was already told.
    pub fn begin(&self) -> Option<u64> {
        if self.running.get() || self.gave_up.get() {
            return None;
        }
        self.running.set(true);
        Some(self.generation.get())
    }

    /// Stops a running loop and clears the alert.
    pub fn cancel(&self) {
        self.generation.set(self.generation.get() + 1);
        self.running.set(false);
        self.gave_up.set(false);
    }

    fn current(&self, generation: u64) -> bool {
        self.generation.get() == generation
    }

    /// `attempt` registers the device once. Its result is `None` when the loop
    /// gave up or was cancelled.
    pub async fn run<T, F, Fut>(
        &self,
        generation: u64,
        client: &ha::Client,
        attempt: F,
    ) -> Option<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = ha::Result<T>>,
    {
        let mut failures = 0;
        let mut delay = DELAYS[0];
        loop {
            if !delay.is_zero() {
                glib::timeout_future(delay).await;
            }
            while self.current(generation) && client.state() != ConnectionState::Connected {
                glib::timeout_future(CONNECTED_POLL).await;
            }
            if !self.current(generation) {
                return None;
            }
            let result = attempt().await;
            if !self.current(generation) {
                return None;
            }
            match result {
                Ok(value) => {
                    log::info!("Registered the device again");
                    self.running.set(false);
                    return Some(value);
                }
                Err(err) if counts(&err) => {
                    failures += 1;
                    log::warn!(
                        "Could not register the device again ({failures} of {}): {err}",
                        DELAYS.len()
                    );
                    if failures == DELAYS.len() {
                        self.running.set(false);
                        self.gave_up.set(true);
                        return None;
                    }
                    delay = DELAYS[failures];
                }
                Err(err) => {
                    log::info!("Could not reach the server to register again: {err}");
                    delay = UNCOUNTED_DELAY;
                }
            }
        }
    }
}

/// A connection that dropped during the attempt is not counted.
fn counts(err: &ha::Error) -> bool {
    !matches!(
        err,
        ha::Error::Network(_) | ha::Error::NotConnected | ha::Error::UntrustedCertificate(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_answered_failures() {
        assert!(counts(&ha::Error::Http {
            status: 500,
            body: String::new()
        }));
        assert!(counts(&ha::Error::RegistrationGone));
        assert!(!counts(&ha::Error::Network("refused".into())));
        assert!(!counts(&ha::Error::NotConnected));
    }

    #[test]
    fn begins_once() {
        let state = Reregistration::default();
        assert!(state.begin().is_some());
        assert!(state.begin().is_none());
        state.cancel();
        assert!(state.begin().is_some());
    }
}
