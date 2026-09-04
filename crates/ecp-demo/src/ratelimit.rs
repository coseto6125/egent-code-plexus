//! Fixed-window per-IP limiter. One process, one map: the demo runs a single
//! instance, so nothing has to be shared across replicas.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct RateLimiter {
    per_window: u32,
    window: Duration,
    state: Mutex<HashMap<IpAddr, (Instant, u32)>>,
}

/// Above this many tracked addresses, a check first drops every expired window.
const PRUNE_ABOVE: usize = 10_000;

impl RateLimiter {
    pub fn per_minute(per_window: u32) -> Self {
        Self::new(per_window, Duration::from_secs(60))
    }

    pub fn new(per_window: u32, window: Duration) -> Self {
        Self {
            per_window,
            window,
            state: Mutex::new(HashMap::new()),
        }
    }

    /// `true` when the request may proceed; the call itself counts.
    pub fn allow(&self, ip: IpAddr) -> bool {
        self.allow_at(ip, Instant::now())
    }

    fn allow_at(&self, ip: IpAddr, now: Instant) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.len() > PRUNE_ABOVE {
            state.retain(|_, (start, _)| now.duration_since(*start) < self.window);
        }
        let entry = state.entry(ip).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= self.per_window
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IP: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

    #[test]
    fn allow_rejects_the_request_after_the_window_quota() {
        let limiter = RateLimiter::new(3, Duration::from_secs(60));
        let now = Instant::now();
        assert!(limiter.allow_at(IP, now));
        assert!(limiter.allow_at(IP, now));
        assert!(limiter.allow_at(IP, now));
        assert!(!limiter.allow_at(IP, now));
    }

    #[test]
    fn allow_resets_the_quota_once_the_window_passes() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let now = Instant::now();
        assert!(limiter.allow_at(IP, now));
        assert!(!limiter.allow_at(IP, now));
        assert!(limiter.allow_at(IP, now + Duration::from_secs(61)));
    }

    #[test]
    fn allow_tracks_addresses_independently() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let other = IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 2));
        let now = Instant::now();
        assert!(limiter.allow_at(IP, now));
        assert!(limiter.allow_at(other, now));
        assert!(!limiter.allow_at(IP, now));
    }
}
