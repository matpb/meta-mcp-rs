//! Token buckets that pace the daemon's writes per platform.

use std::time::{Duration, Instant};

pub const ENV_SEND_INTERVAL_MS: &str = "META_MCP_PERSONAL_SEND_INTERVAL_MS";
pub const ENV_SEND_BURST: &str = "META_MCP_PERSONAL_SEND_BURST";
const DEFAULT_SEND_INTERVAL: Duration = Duration::from_secs(2);
const DEFAULT_SEND_BURST: u32 = 3;
const LIGHT_INTERVAL: Duration = Duration::from_secs(1);
const LIGHT_BURST: u32 = 5;

/// Which bucket an op draws from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaceClass {
    /// New messages: `send_text` and `send_media`.
    Send,
    /// Reactions, read marks and typing indicators.
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaceConfig {
    pub send_interval: Duration,
    pub send_burst: u32,
}

impl Default for PaceConfig {
    fn default() -> Self {
        Self {
            send_interval: DEFAULT_SEND_INTERVAL,
            send_burst: DEFAULT_SEND_BURST,
        }
    }
}

impl PaceConfig {
    /// Reads both variables; an empty, zero or unparsable value keeps the default.
    pub fn from_env() -> Self {
        Self::parse(
            std::env::var(ENV_SEND_INTERVAL_MS).ok().as_deref(),
            std::env::var(ENV_SEND_BURST).ok().as_deref(),
        )
    }

    pub fn parse(interval_ms: Option<&str>, burst: Option<&str>) -> Self {
        let default = Self::default();
        let positive = |raw: Option<&str>| {
            raw.and_then(|s| s.trim().parse::<u32>().ok())
                .filter(|n| *n > 0)
        };
        Self {
            send_interval: positive(interval_ms).map_or(default.send_interval, |ms| {
                Duration::from_millis(u64::from(ms))
            }),
            send_burst: positive(burst).unwrap_or(default.send_burst),
        }
    }
}

/// A bucket holding up to `capacity` tokens and gaining one per `interval`.
#[derive(Debug, Clone)]
pub struct Bucket {
    capacity: f64,
    interval: Duration,
    tokens: f64,
    last: Instant,
}

impl Bucket {
    pub fn new(capacity: u32, interval: Duration, now: Instant) -> Self {
        Self {
            capacity: f64::from(capacity),
            interval,
            tokens: f64::from(capacity),
            last: now,
        }
    }

    /// Takes one token, or says how long until one is available.
    pub fn take(&mut self, now: Instant) -> Result<(), Duration> {
        let elapsed = now.saturating_duration_since(self.last);
        self.last = now;
        self.tokens =
            (self.tokens + elapsed.as_secs_f64() / self.interval.as_secs_f64()).min(self.capacity);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Ok(())
        } else {
            Err(self.interval.mul_f64(1.0 - self.tokens))
        }
    }
}

/// The two buckets of one platform.
#[derive(Debug)]
pub struct Pace {
    send: Bucket,
    light: Bucket,
}

impl Pace {
    pub fn new(cfg: PaceConfig, now: Instant) -> Self {
        Self {
            send: Bucket::new(cfg.send_burst, cfg.send_interval, now),
            light: Bucket::new(LIGHT_BURST, LIGHT_INTERVAL, now),
        }
    }

    pub fn take(&mut self, class: PaceClass, now: Instant) -> Result<(), Duration> {
        match class {
            PaceClass::Send => self.send.take(now),
            PaceClass::Light => self.light.take(now),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_burst_passes_then_tokens_return_one_per_interval() {
        let t0 = Instant::now();
        let mut b = Bucket::new(3, ms(2000), t0);
        assert!(b.take(t0).is_ok() && b.take(t0).is_ok() && b.take(t0).is_ok());
        assert_eq!(b.take(t0), Err(ms(2000)));
        assert_eq!(b.take(t0 + ms(500)), Err(ms(1500)));
        assert!(b.take(t0 + ms(2000)).is_ok());
        assert!(b.take(t0 + ms(2000)).is_err());
        assert!(
            b.take(t0 + ms(60_000)).is_ok(),
            "refill is capped at the burst"
        );
        assert!(b.take(t0 + ms(60_000)).is_ok() && b.take(t0 + ms(60_000)).is_ok());
        assert!(b.take(t0 + ms(60_000)).is_err());
    }

    #[test]
    fn classes_do_not_share_tokens() {
        let t0 = Instant::now();
        let mut p = Pace::new(PaceConfig::default(), t0);
        for _ in 0..3 {
            assert!(p.take(PaceClass::Send, t0).is_ok());
        }
        assert!(p.take(PaceClass::Send, t0).is_err());
        for _ in 0..5 {
            assert!(p.take(PaceClass::Light, t0).is_ok());
        }
        let wait = p.take(PaceClass::Light, t0).expect_err("sixth is paced");
        assert_eq!(wait, ms(1000));
    }

    #[test]
    fn config_defaults_and_overrides() {
        let d = PaceConfig::default();
        assert_eq!((d.send_interval, d.send_burst), (ms(2000), 3));
        let c = PaceConfig::parse(Some("250"), Some("7"));
        assert_eq!((c.send_interval, c.send_burst), (ms(250), 7));
        for bad in [None, Some(""), Some("0"), Some("x"), Some("-1")] {
            assert_eq!(PaceConfig::parse(bad, bad), d, "{bad:?}");
        }
    }
}
