//! omp's double-tap (←← opens and closes the agents hub): the second tap 40–500 ms after
//! the first. Faster taps are key repeat or a terminal burst and never count.

use std::{sync::LazyLock, time::Instant};

use rune::Tx;

/// Tracks the last unpaired tap of one key.
#[derive(Debug, Default)]
pub struct DoubleTap {
    /// When the pending first tap happened, in seconds (see [`tap_time`]).
    last: Option<f64>,
}

impl DoubleTap {
    const MIN_GAP: f64 = 0.040;
    const MAX_GAP: f64 = 0.500;

    /// A tap at `now` seconds; true when it completes a double-tap (the pair is consumed).
    pub fn tap(&mut self, now: f64) -> bool {
        let fired = self.last.is_some_and(|last| {
            let gap = (now - last).max(0.0);
            (Self::MIN_GAP..Self::MAX_GAP).contains(&gap)
        });
        self.last = (!fired).then_some(now);
        fired
    }

    /// Another key came between taps.
    pub fn reset(&mut self) {
        self.last = None;
    }
}

/// The time of the key a handler runs for, in seconds for [`DoubleTap::tap`]: when the
/// window took it (so taps piled up between frames keep their own times), else `clock`
/// (the app clock, `s.clock.t`; keys fed to a headless `Runtime` carry no input time).
pub fn tap_time(tx: &Tx, clock: f64) -> f64 {
    static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);
    match tx.input_time() {
        Some(at) => at.saturating_duration_since(*EPOCH).as_secs_f64(),
        None => clock,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn double_tap_needs_a_second_tap_within_the_window() {
        let ms = |n: u32| f64::from(n) / 1000.0;
        let mut tap = DoubleTap::default();
        assert!(!tap.tap(0.0));
        assert!(tap.tap(ms(200)));
        // The pair is consumed: the next tap starts over.
        assert!(!tap.tap(ms(300)));

        // Too slow: the late tap becomes the first of a new pair.
        let mut tap = DoubleTap::default();
        assert!(!tap.tap(0.0));
        assert!(!tap.tap(ms(500)));
        assert!(tap.tap(ms(600)));

        // Key-repeat speed never fires.
        let mut tap = DoubleTap::default();
        assert!(!tap.tap(0.0));
        assert!(!tap.tap(ms(20)));
        assert!(!tap.tap(ms(39)));

        // Another key in between cancels.
        let mut tap = DoubleTap::default();
        assert!(!tap.tap(0.0));
        tap.reset();
        assert!(!tap.tap(ms(100)));
    }
}
