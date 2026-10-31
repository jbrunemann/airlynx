//! Whether a transfer or browse session is running, so a phone can hold its wake and Wi-Fi locks only then
//! instead of the whole time it listens.

use std::sync::{Mutex, OnceLock};

/// Sessions running now, in either direction.
static ACTIVE: Mutex<usize> = Mutex::new(0);
static LISTENER: OnceLock<Box<dyn Fn(bool) + Send + Sync>> = OnceLock::new();

/// Calls `listener` with true when the first session starts and with false when the last one ends. Only the first
/// listener set counts.
pub fn on_change(listener: impl Fn(bool) + Send + Sync + 'static) {
    let _ = LISTENER.set(Box::new(listener));
}

/// Counts as a running session while it lives.
pub(crate) struct Busy(());

impl Busy {
    pub(crate) fn new() -> Self {
        let mut active = ACTIVE.lock().unwrap();
        *active += 1;
        // Called under the lock, so a start and an end can't reach the listener out of order.
        if *active == 1 && let Some(listener) = LISTENER.get() {
            listener(true);
        }
        Self(())
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        let mut active = ACTIVE.lock().unwrap();
        *active -= 1;
        if *active == 0 && let Some(listener) = LISTENER.get() {
            listener(false);
        }
    }
}
