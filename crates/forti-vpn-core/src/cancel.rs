//! A cancellation flag that worker threads can poll or wait on.
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<(Mutex<bool>, Condvar)>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        let (flag, condvar) = &*self.0;
        *flag.lock().unwrap_or_else(PoisonError::into_inner) = true;
        condvar.notify_all();
    }

    pub fn is_cancelled(&self) -> bool {
        *self.0.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sleep for up to `timeout`, waking early when cancelled. Returns
    /// whether the token is cancelled.
    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        let (flag, condvar) = &*self.0;
        let guard = flag.lock().unwrap_or_else(PoisonError::into_inner);
        let (guard, _) = condvar
            .wait_timeout_while(guard, timeout, |cancelled| !*cancelled)
            .unwrap_or_else(PoisonError::into_inner);
        *guard
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn wait_wakes_on_cancel() {
        let token = CancelToken::new();
        assert!(!token.wait_timeout(Duration::from_millis(5)));
        let remote = token.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            remote.cancel();
        });
        let started = Instant::now();
        assert!(token.wait_timeout(Duration::from_secs(5)));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(token.is_cancelled());
        thread.join().unwrap();
    }
}
