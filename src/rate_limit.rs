use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

pub struct RateLimiter {
    register: Mutex<HashMap<IpAddr, Vec<Instant>>>,
    lookup:   Mutex<HashMap<IpAddr, Vec<Instant>>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            register: Mutex::new(HashMap::new()),
            lookup:   Mutex::new(HashMap::new()),
        }
    }

    /// 5 requests per IP per hour
    pub async fn allow_register(&self, ip: IpAddr) -> bool {
        self.check(&self.register, ip, 5, Duration::from_secs(3600)).await
    }

    /// 60 requests per IP per minute
    pub async fn allow_lookup(&self, ip: IpAddr) -> bool {
        self.check(&self.lookup, ip, 60, Duration::from_secs(60)).await
    }

    async fn check(
        &self,
        map:    &Mutex<HashMap<IpAddr, Vec<Instant>>>,
        ip:     IpAddr,
        limit:  usize,
        window: Duration,
    ) -> bool {
        let mut guard = map.lock().await;
        let now = Instant::now();
        let times = guard.entry(ip).or_default();
        times.retain(|t| now.duration_since(*t) < window);
        if times.len() >= limit {
            return false;
        }
        times.push(now);
        true
    }
}
