use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

use crate::config::ModelEndpoint;

/// Per-endpoint rate limiter: concurrency, RPM, TPM.
#[derive(Clone, Debug)]
pub struct RateLimiter {
    semaphore: Option<Arc<Semaphore>>,
    rpm_limit: usize,
    tpm_limit: usize,
    rpm_timestamps: Arc<tokio::sync::Mutex<Vec<Instant>>>,
    tpm_bucket: Arc<tokio::sync::Mutex<TokenBucket>>,
}

#[derive(Debug)]
struct TokenBucket {
    tokens: f64,
    last_update: Instant,
}

impl RateLimiter {
    fn available_tokens(&self, bucket: &TokenBucket, now: Instant) -> f64 {
        let elapsed = now.duration_since(bucket.last_update).as_secs_f64();
        let refill_rate = self.tpm_limit as f64 / 60.0;
        (bucket.tokens + elapsed * refill_rate).min(self.tpm_limit as f64)
    }

    pub fn new(concurrency: usize, rpm: usize, tpm: usize) -> Self {
        let semaphore = if concurrency > 0 {
            Some(Arc::new(Semaphore::new(concurrency)))
        } else {
            None
        };
        Self {
            semaphore,
            rpm_limit: rpm,
            tpm_limit: tpm,
            rpm_timestamps: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            tpm_bucket: Arc::new(tokio::sync::Mutex::new(TokenBucket {
                tokens: tpm as f64,
                last_update: Instant::now(),
            })),
        }
    }

    /// Build a rate limiter from a model endpoint's configured limits.
    /// Returns `None` if all limits are zero (unlimited).
    pub fn for_endpoint(endpoint: &ModelEndpoint) -> Option<Self> {
        if endpoint.concurrency > 0 || endpoint.rpm > 0 || endpoint.tpm > 0 {
            tracing::info!(
                "Rate limiter for {}: concurrency={}, rpm={}, tpm={}",
                endpoint.model,
                endpoint.concurrency,
                endpoint.rpm,
                endpoint.tpm
            );
            Some(Self::new(endpoint.concurrency, endpoint.rpm, endpoint.tpm))
        } else {
            None
        }
    }

    /// Acquire a permit for a request, respecting concurrency + RPM.
    /// Returns when it's safe to proceed.
    pub async fn acquire(
        &self,
        estimated_tokens: usize,
    ) -> Result<RatePermit, crate::error::PaperedError> {
        // 1. RPM: sliding window (no semaphore held during waits)
        if self.rpm_limit > 0 {
            let mut window = self.rpm_timestamps.lock().await;
            let mut now = Instant::now();
            let mut cutoff = now - Duration::from_secs(60);
            window.retain(|t| *t > cutoff);
            while window.len() >= self.rpm_limit {
                let oldest = window[0];
                let wait = (oldest + Duration::from_secs(60)) - now;
                drop(window);
                tokio::time::sleep(wait).await;
                // Refresh timestamps after sleep
                window = self.rpm_timestamps.lock().await;
                now = Instant::now();
                cutoff = now - Duration::from_secs(60);
                window.retain(|t| *t > cutoff);
            }
            window.push(now);
        }

        // TPM: token bucket
        if self.tpm_limit > 0 && estimated_tokens > 0 {
            let mut bucket = self.tpm_bucket.lock().await;
            let mut now = Instant::now();
            let tpm_f = self.tpm_limit as f64;
            let refill_rate = tpm_f / 60.0; // tokens per second
            bucket.tokens = self.available_tokens(&bucket, now);
            bucket.last_update = now;

            let needed = estimated_tokens as f64;
            while bucket.tokens < needed {
                let deficit = needed - bucket.tokens;
                let wait_secs = deficit / refill_rate;
                drop(bucket);
                tokio::time::sleep(Duration::from_secs_f64(wait_secs)).await;
                // Re-acquire and refresh after sleep
                bucket = self.tpm_bucket.lock().await;
                now = Instant::now();
                bucket.tokens = self.available_tokens(&bucket, now);
                bucket.last_update = now;
            }
            bucket.tokens -= needed;
        }

        // 2. Concurrency: acquire semaphore permit last (don't hold it during rate waits)
        let _permit = if let Some(ref sem) = self.semaphore {
            Some(sem.clone().acquire_owned().await.map_err(|e| {
                crate::error::PaperedError::config(format!("Rate limiter semaphore closed: {e}"))
            })?)
        } else {
            None
        };

        Ok(RatePermit { _permit })
    }

    /// Quick check without waiting (returns false if limits would be exceeded).
    #[cfg(test)]
    pub async fn would_block(&self, estimated_tokens: usize) -> bool {
        if let Some(ref sem) = self.semaphore
            && sem.available_permits() == 0
        {
            return true;
        }
        if self.rpm_limit > 0 {
            let window = self.rpm_timestamps.lock().await;
            let now = Instant::now();
            let cutoff = now - Duration::from_secs(60);
            let recent = window.iter().copied().filter(|t| *t > cutoff).count();
            if recent >= self.rpm_limit {
                return true;
            }
        }
        if self.tpm_limit > 0 && estimated_tokens > 0 {
            let bucket = self.tpm_bucket.lock().await;
            let now = Instant::now();
            let available = self.available_tokens(&bucket, now);
            if available < estimated_tokens as f64 {
                return true;
            }
        }
        false
    }
}

/// RAII guard returned by `RateLimiter::acquire`. Permit is released on drop.
pub struct RatePermit {
    _permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_rate_limiter_concurrency() {
        let limiter = RateLimiter::new(2, 0, 0);
        let p1 = limiter.acquire(0).await.unwrap();
        let p2 = limiter.acquire(0).await.unwrap();
        assert!(limiter.would_block(0).await);
        drop(p1);
        // Give a moment for the permit to be released
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!limiter.would_block(0).await);
        drop(p2);
    }

    #[tokio::test]
    async fn test_rate_limiter_rpm_timestamps() {
        let limiter = RateLimiter::new(0, 2, 0);
        assert!(!limiter.would_block(0).await);
        let _ = limiter.acquire(0).await.unwrap();
        assert!(!limiter.would_block(0).await);
        let _ = limiter.acquire(0).await.unwrap();
        assert!(limiter.would_block(0).await);
    }

    #[tokio::test]
    async fn test_rate_limiter_tpm_bucket() {
        let limiter = RateLimiter::new(0, 0, 100);
        assert!(!limiter.would_block(50).await);
        let _ = limiter.acquire(80).await.unwrap();
        // 20 tokens remain
        assert!(!limiter.would_block(20).await);
        assert!(limiter.would_block(30).await);
    }

    #[tokio::test]
    async fn test_rate_limiter_unlimited() {
        let limiter = RateLimiter::new(0, 0, 0);
        for _ in 0..10 {
            let _ = limiter.acquire(0).await.unwrap();
        }
        assert!(!limiter.would_block(0).await);
    }

    fn test_endpoint(concurrency: usize, rpm: usize, tpm: usize) -> ModelEndpoint {
        ModelEndpoint {
            api_base: "http://localhost".to_string(),
            api_key: None,
            model: "test".to_string(),
            concurrency,
            rpm,
            tpm,
            extra_body: None,
            reasoning_effort: None,
            context_window: None,
            max_output_tokens: None,
        }
    }

    #[test]
    fn test_for_endpoint_returns_none_when_unlimited() {
        assert!(RateLimiter::for_endpoint(&test_endpoint(0, 0, 0)).is_none());
    }

    #[test]
    fn test_for_endpoint_returns_some_when_limited() {
        assert!(RateLimiter::for_endpoint(&test_endpoint(1, 0, 0)).is_some());
    }
}
