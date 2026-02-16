use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::fmt;

const CLEANUP_IDLE_SECS: i64 = 7_200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimitConfig {
    pub requests_per_minute: u32,
    pub requests_per_hour: u32,
    pub burst_size: u32,
    pub per_endpoint_limits: HashMap<String, u32>,
    pub per_model_limits: HashMap<String, u32>,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            requests_per_minute: 60,
            requests_per_hour: 1_000,
            burst_size: 10,
            per_endpoint_limits: HashMap::new(),
            per_model_limits: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TokenBucket {
    pub tokens: f64,
    pub max_tokens: f64,
    pub refill_rate: f64,
    pub last_refill: DateTime<Utc>,
}

impl TokenBucket {
    pub fn new(max_tokens: f64, refill_rate: f64) -> Self {
        Self {
            tokens: max_tokens,
            max_tokens,
            refill_rate,
            last_refill: Utc::now(),
        }
    }

    pub fn try_consume(&mut self, n: f64) -> bool {
        self.refill();
        if self.tokens >= n {
            self.tokens -= n;
            true
        } else {
            false
        }
    }

    pub fn refill(&mut self) -> f64 {
        let now = Utc::now();
        self.refill_at(now)
    }

    fn refill_at(&mut self, now: DateTime<Utc>) -> f64 {
        let elapsed_secs = (now - self.last_refill)
            .to_std()
            .map_or(0.0, |d| d.as_secs_f64());

        if elapsed_secs <= 0.0 {
            return self.tokens;
        }

        let added = elapsed_secs * self.refill_rate;
        self.tokens = (self.tokens + added).min(self.max_tokens);
        self.last_refill = now;
        self.tokens
    }

    fn consume_at(&mut self, n: f64, now: DateTime<Utc>) -> bool {
        self.refill_at(now);
        if self.tokens >= n {
            self.tokens -= n;
            true
        } else {
            false
        }
    }

    fn remaining_whole_tokens_at(&mut self, now: DateTime<Utc>) -> u32 {
        self.refill_at(now);
        self.tokens.floor() as u32
    }

    fn retry_after_secs(&self, needed: f64) -> f64 {
        if self.tokens >= needed {
            0.0
        } else {
            (needed - self.tokens) / self.refill_rate
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RateLimitResult {
    Allowed { remaining: u32 },
    Limited { retry_after_secs: f64 },
}

impl fmt::Display for RateLimitResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Allowed { remaining } => write!(f, "allowed ({remaining} remaining)"),
            Self::Limited { retry_after_secs } => {
                write!(f, "limited (retry after {:.3}s)", retry_after_secs)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateLimitError {
    ConfigError(String),
    BucketNotFound(String),
}

impl fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConfigError(msg) => write!(f, "rate limiter config error: {msg}"),
            Self::BucketNotFound(key) => write!(f, "rate limiter bucket not found: {key}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RateLimiter {
    pub config: RateLimitConfig,
    pub buckets: HashMap<String, TokenBucket>,
}

impl RateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            buckets: HashMap::new(),
        }
    }

    pub fn check_rate_limit(&mut self, key: &str) -> RateLimitResult {
        self.check_rate_limit_at(key, Utc::now())
    }

    pub fn record_request(&mut self, key: &str) {
        let now = Utc::now();
        let bucket = self.bucket_for_key(key);
        let _ = bucket.consume_at(1.0, now);
    }

    pub fn remaining_tokens(&mut self, key: &str) -> u32 {
        let now = Utc::now();
        let bucket = self.bucket_for_key(key);
        bucket.remaining_whole_tokens_at(now)
    }

    pub fn reset(&mut self, key: &str) {
        let spec = self.bucket_spec_for_key(key);
        self.buckets.insert(key.to_string(), TokenBucket::new(spec.0, spec.1));
    }

    pub fn reset_all(&mut self) {
        self.buckets.clear();
    }

    pub fn cleanup_expired(&mut self) {
        let now = Utc::now();
        self.buckets.retain(|_, bucket| {
            let idle_secs = (now - bucket.last_refill).num_seconds();
            idle_secs < CLEANUP_IDLE_SECS
        });
    }

    fn check_rate_limit_at(&mut self, key: &str, now: DateTime<Utc>) -> RateLimitResult {
        let bucket = self.bucket_for_key(key);
        bucket.refill_at(now);

        if bucket.tokens >= 1.0 {
            let remaining = bucket.tokens.floor() as u32;
            RateLimitResult::Allowed { remaining }
        } else {
            RateLimitResult::Limited {
                retry_after_secs: bucket.retry_after_secs(1.0),
            }
        }
    }

    fn bucket_for_key(&mut self, key: &str) -> &mut TokenBucket {
        let spec = self.bucket_spec_for_key(key);
        self.buckets
            .entry(key.to_string())
            .or_insert_with(|| TokenBucket::new(spec.0, spec.1))
    }

    fn bucket_spec_for_key(&self, key: &str) -> (f64, f64) {
        let rpm = self
            .config
            .per_endpoint_limits
            .get(key)
            .copied()
            .or_else(|| self.config.per_model_limits.get(key).copied())
            .unwrap_or(self.config.requests_per_minute)
            .max(1);

        let effective_rps = (rpm as f64 / 60.0).min(self.config.requests_per_hour.max(1) as f64 / 3_600.0);
        let max_tokens = self
            .config
            .burst_size
            .max(1)
            .min(rpm)
            .min(self.config.requests_per_hour.max(1)) as f64;

        (max_tokens, effective_rps.max(f64::EPSILON))
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new(RateLimitConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn mk_config() -> RateLimitConfig {
        let mut cfg = RateLimitConfig {
            requests_per_minute: 60,
            requests_per_hour: 10_000,
            burst_size: 5,
            per_endpoint_limits: HashMap::new(),
            per_model_limits: HashMap::new(),
        };
        cfg.per_endpoint_limits.insert("endpoint:/slow".to_string(), 30);
        cfg.per_model_limits.insert("model:claude".to_string(), 20);
        cfg
    }

    #[test]
    fn rate_limit_config_defaults() {
        let cfg = RateLimitConfig::default();
        assert_eq!(cfg.requests_per_minute, 60);
        assert_eq!(cfg.requests_per_hour, 1_000);
        assert_eq!(cfg.burst_size, 10);
        assert!(cfg.per_endpoint_limits.is_empty());
        assert!(cfg.per_model_limits.is_empty());
    }

    #[test]
    fn token_bucket_refill_adds_tokens_based_on_elapsed_time() {
        let mut bucket = TokenBucket::new(10.0, 1.0);
        bucket.tokens = 0.0;
        bucket.last_refill = Utc::now() - Duration::seconds(3);
        bucket.refill();
        assert!(bucket.tokens >= 2.9);
    }

    #[test]
    fn token_bucket_refill_does_not_overflow_max() {
        let mut bucket = TokenBucket::new(5.0, 100.0);
        bucket.tokens = 1.0;
        bucket.last_refill = Utc::now() - Duration::seconds(10);
        bucket.refill();
        assert_eq!(bucket.tokens, 5.0);
    }

    #[test]
    fn token_bucket_try_consume_succeeds_when_tokens_available() {
        let mut bucket = TokenBucket::new(2.0, 0.0);
        assert!(bucket.try_consume(1.0));
        assert_eq!(bucket.tokens, 1.0);
    }

    #[test]
    fn token_bucket_try_consume_fails_when_underflow() {
        let mut bucket = TokenBucket::new(1.0, 0.0);
        assert!(!bucket.try_consume(2.0));
        assert_eq!(bucket.tokens, 1.0);
    }

    #[test]
    fn check_rate_limit_allowed_by_default() {
        let mut limiter = RateLimiter::new(mk_config());
        let result = limiter.check_rate_limit("client:A");
        assert!(matches!(result, RateLimitResult::Allowed { .. }));
    }

    #[test]
    fn record_request_consumes_token() {
        let mut limiter = RateLimiter::new(mk_config());
        let before = limiter.remaining_tokens("client:B");
        limiter.record_request("client:B");
        let after = limiter.remaining_tokens("client:B");
        assert!(after < before);
    }

    #[test]
    fn limiter_reports_limited_after_burst_exhausted() {
        let mut cfg = mk_config();
        cfg.burst_size = 2;
        cfg.requests_per_minute = 2;
        cfg.requests_per_hour = 100;
        let mut limiter = RateLimiter::new(cfg);

        limiter.record_request("client:C");
        limiter.record_request("client:C");
        let result = limiter.check_rate_limit("client:C");
        assert!(matches!(result, RateLimitResult::Limited { .. }));
    }

    #[test]
    fn burst_handling_allows_multiple_fast_requests() {
        let mut cfg = mk_config();
        cfg.burst_size = 4;
        let mut limiter = RateLimiter::new(cfg);

        for _ in 0..4 {
            assert!(matches!(
                limiter.check_rate_limit("client:D"),
                RateLimitResult::Allowed { .. }
            ));
            limiter.record_request("client:D");
        }
    }

    #[test]
    fn per_endpoint_limit_changes_bucket_capacity() {
        let mut limiter = RateLimiter::new(mk_config());
        let _ = limiter.remaining_tokens("endpoint:/slow");
        let bucket = limiter
            .buckets
            .get("endpoint:/slow")
            .expect("bucket should exist");
        assert_eq!(bucket.max_tokens, 5.0_f64.min(30.0).min(10_000.0));
        assert!((bucket.refill_rate - (30.0 / 60.0)).abs() < 1e-9);
    }

    #[test]
    fn per_model_limit_changes_refill_rate() {
        let mut limiter = RateLimiter::new(mk_config());
        let _ = limiter.remaining_tokens("model:claude");
        let bucket = limiter
            .buckets
            .get("model:claude")
            .expect("bucket should exist");
        assert!((bucket.refill_rate - (20.0 / 60.0)).abs() < 1e-9);
    }

    #[test]
    fn reset_restores_bucket_to_full_tokens() {
        let mut limiter = RateLimiter::new(mk_config());
        limiter.record_request("client:E");
        limiter.reset("client:E");
        let remaining = limiter.remaining_tokens("client:E");
        assert_eq!(remaining, 5);
    }

    #[test]
    fn reset_all_clears_all_buckets() {
        let mut limiter = RateLimiter::new(mk_config());
        let _ = limiter.remaining_tokens("one");
        let _ = limiter.remaining_tokens("two");
        assert_eq!(limiter.buckets.len(), 2);
        limiter.reset_all();
        assert!(limiter.buckets.is_empty());
    }

    #[test]
    fn cleanup_expired_removes_stale_buckets() {
        let mut limiter = RateLimiter::new(mk_config());
        let _ = limiter.remaining_tokens("stale");
        let _ = limiter.remaining_tokens("fresh");

        if let Some(bucket) = limiter.buckets.get_mut("stale") {
            bucket.last_refill = Utc::now() - Duration::seconds(CLEANUP_IDLE_SECS + 1);
        }

        limiter.cleanup_expired();

        assert!(!limiter.buckets.contains_key("stale"));
        assert!(limiter.buckets.contains_key("fresh"));
    }

    #[test]
    fn remaining_tokens_reports_floor_value() {
        let mut limiter = RateLimiter::new(mk_config());
        let _ = limiter.remaining_tokens("client:F");
        if let Some(bucket) = limiter.buckets.get_mut("client:F") {
            bucket.tokens = 2.9;
        }

        assert_eq!(limiter.remaining_tokens("client:F"), 2);
    }

    #[test]
    fn limited_result_reports_positive_retry_after() {
        let mut cfg = mk_config();
        cfg.burst_size = 1;
        cfg.requests_per_minute = 1;
        cfg.requests_per_hour = 10;
        let mut limiter = RateLimiter::new(cfg);
        limiter.record_request("client:G");

        let result = limiter.check_rate_limit("client:G");
        match result {
            RateLimitResult::Limited { retry_after_secs } => assert!(retry_after_secs > 0.0),
            _ => panic!("expected limited result"),
        }
    }

    #[test]
    fn rate_limit_result_formatting_is_readable() {
        let allowed = RateLimitResult::Allowed { remaining: 4 }.to_string();
        let limited = RateLimitResult::Limited {
            retry_after_secs: 2.5,
        }
        .to_string();

        assert!(allowed.contains("allowed"));
        assert!(limited.contains("retry after"));
    }

    #[test]
    fn rate_limit_error_formatting_is_readable() {
        let config_err = RateLimitError::ConfigError("bad".to_string()).to_string();
        let bucket_err = RateLimitError::BucketNotFound("k".to_string()).to_string();
        assert!(config_err.contains("bad"));
        assert!(bucket_err.contains("k"));
    }
}
