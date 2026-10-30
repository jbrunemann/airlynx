//! A stop signal that background loops sleep on, so they wake when stopped instead of polling a flag.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[derive(Clone, Default)]
pub(crate) struct Stop(Arc<(Mutex<bool>, Condvar)>);

impl Stop {
    pub(crate) fn set(&self) {
        let (stopped, wake) = &*self.0;
        *stopped.lock().unwrap() = true;
        wake.notify_all();
    }

    pub(crate) fn is_set(&self) -> bool {
        *self.0.0.lock().unwrap()
    }

    /// Sleeps for `timeout` or until stopped, and returns whether it was stopped.
    pub(crate) fn wait(&self, timeout: Duration) -> bool {
        let (stopped, wake) = &*self.0;
        let guard = stopped.lock().unwrap();
        *wake.wait_timeout_while(guard, timeout, |stopped| !*stopped).unwrap().0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Instant;

    #[test]
    fn a_wait_ends_when_stopped() {
        let stop = Stop::default();
        assert!(!stop.wait(Duration::from_millis(1)));
        let waiter = {
            let stop = stop.clone();
            thread::spawn(move || stop.wait(Duration::from_secs(10)))
        };
        let started = Instant::now();
        stop.set();
        assert!(waiter.join().unwrap());
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(stop.is_set());
    }
}
