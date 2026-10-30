//! Runtime stall watchdog (0.10.4). A task on the core's async runtime stamps
//! a tick every 250 ms; a plain OS thread, which no runtime stall can hold,
//! reads the stamp and logs when it falls behind — "the core's runtime was
//! unresponsive for N s", then how long the stall lasted once it ends. A
//! stalled runtime is what bridges read as a dead core (Problem
//! 00fkvifk0v8a), and until now it left no trace in core.log.
//!
//! `Instant` is monotonic and does not advance while the machine sleeps
//! (mach_absolute_time on macOS, CLOCK_MONOTONIC on Linux), so a laptop lid
//! is not a stall.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(250);
const CHECK: Duration = Duration::from_millis(500);

/// What one observation of the runtime's lag means.
#[derive(Debug, PartialEq)]
pub enum StallEvent {
    /// The lag just crossed the threshold.
    Stalled(Duration),
    /// The runtime ticks again; the longest lag seen during the stall.
    Recovered(Duration),
}

/// The stall state machine, apart from clocks and threads so it tests alone.
pub struct StallTracker {
    threshold: Duration,
    /// The longest lag of the stall in progress, if one is.
    worst: Option<Duration>,
}

impl StallTracker {
    pub fn new(threshold: Duration) -> Self {
        Self {
            threshold,
            worst: None,
        }
    }

    pub fn observe(&mut self, lag: Duration) -> Option<StallEvent> {
        match self.worst {
            None if lag >= self.threshold => {
                self.worst = Some(lag);
                Some(StallEvent::Stalled(lag))
            }
            None => None,
            Some(worst) if lag >= self.threshold => {
                self.worst = Some(worst.max(lag));
                None
            }
            Some(worst) => {
                self.worst = None;
                Some(StallEvent::Recovered(worst))
            }
        }
    }
}

/// `ENGRAM_STALL_WARN_SECS` (default 5; 0 turns the watchdog off).
fn threshold() -> Option<Duration> {
    let secs = std::env::var("ENGRAM_STALL_WARN_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or(5.0);
    (secs > 0.0).then(|| Duration::from_secs_f64(secs))
}

/// Start the ticker on the current runtime and the checker thread beside it.
/// Returns the ticker so shutdown can abort it.
pub fn spawn() -> Option<tokio::task::JoinHandle<()>> {
    let threshold = threshold()?;
    let origin = Instant::now();
    let stamp = Arc::new(AtomicU64::new(0));
    let ticker = {
        let stamp = stamp.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(TICK);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                stamp.store(origin.elapsed().as_millis() as u64, Ordering::Relaxed);
            }
        })
    };
    let spawned = std::thread::Builder::new()
        .name("engram-watchdog".into())
        .spawn(move || {
            let mut tracker = StallTracker::new(threshold);
            loop {
                std::thread::sleep(CHECK);
                let now = origin.elapsed().as_millis() as u64;
                let lag = Duration::from_millis(now.saturating_sub(stamp.load(Ordering::Relaxed)));
                match tracker.observe(lag) {
                    Some(StallEvent::Stalled(lag)) => tracing::warn!(
                        "the core's runtime has been unresponsive for {:.1}s — requests and \
                         bridge pings are waiting",
                        lag.as_secs_f64()
                    ),
                    Some(StallEvent::Recovered(worst)) => tracing::warn!(
                        "the core's runtime responds again after a {:.1}s stall",
                        worst.as_secs_f64()
                    ),
                    None => {}
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("runtime watchdog not started: {e}");
    }
    Some(ticker)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Duration = Duration::from_secs(5);

    fn s(secs: f64) -> Duration {
        Duration::from_secs_f64(secs)
    }

    #[test]
    fn quiet_runtime_reports_nothing() {
        let mut t = StallTracker::new(T);
        for lag in [0.0, 0.3, 1.0, 4.9] {
            assert_eq!(t.observe(s(lag)), None);
        }
    }

    #[test]
    fn a_stall_is_reported_once_then_its_worst_lag_on_recovery() {
        let mut t = StallTracker::new(T);
        assert_eq!(t.observe(s(0.2)), None);
        assert_eq!(t.observe(s(5.0)), Some(StallEvent::Stalled(s(5.0))));
        assert_eq!(
            t.observe(s(9.5)),
            None,
            "one warning per stall, not one per check"
        );
        assert_eq!(t.observe(s(12.0)), None);
        assert_eq!(t.observe(s(0.25)), Some(StallEvent::Recovered(s(12.0))));
        assert_eq!(t.observe(s(0.25)), None);
    }

    #[test]
    fn a_second_stall_starts_fresh() {
        let mut t = StallTracker::new(T);
        t.observe(s(20.0));
        t.observe(s(0.1));
        assert_eq!(t.observe(s(6.0)), Some(StallEvent::Stalled(s(6.0))));
        assert_eq!(t.observe(s(0.1)), Some(StallEvent::Recovered(s(6.0))));
    }
}
