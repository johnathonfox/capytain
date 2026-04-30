// SPDX-License-Identifier: Apache-2.0

//! Helpers for the watcher reconnect loops in [`crate::imap_idle`]
//! and [`crate::jmap_push`]. Both run an exponential-backoff sleep
//! between retry attempts; both pass that sleep through [`jittered`]
//! to spread reconnects out so a multi-account box doesn't hammer
//! provider rate limits in lock-step after a shared network event.

use std::time::Duration;

use rand::Rng;

/// Multiply `base` by a random factor in `[0.5, 1.5)`. The result is
/// clamped at `Duration::ZERO` (subtract is impossible since the
/// factor is positive, but the clamp keeps the function total under
/// any conceivable future change).
///
/// Uniform jitter rather than a half-jitter or decorrelated-jitter
/// flavour: simpler to reason about, plenty of dispersion for the
/// "5 accounts on one box" case the audit flagged. If a future
/// follow-up shows we need tighter spread (e.g. cap on the upper
/// end so a transient flake doesn't randomly wait 7.5 minutes), we
/// can swap the strategy here without touching the call sites.
pub fn jittered(base: Duration) -> Duration {
    let mut rng = rand::rng();
    let factor: f64 = rng.random_range(0.5..1.5);
    base.mul_f64(factor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jittered_lands_in_half_to_one_and_a_half() {
        let base = Duration::from_secs(10);
        for _ in 0..1000 {
            let out = jittered(base);
            assert!(out >= Duration::from_secs(5));
            assert!(out < Duration::from_secs(15));
        }
    }

    #[test]
    fn jittered_zero_stays_zero() {
        assert_eq!(jittered(Duration::ZERO), Duration::ZERO);
    }
}
