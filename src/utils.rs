/// Returns timestamp in ns
#[cfg(unix)]
#[inline(never)]
pub fn mono_time_ns() -> u64 {
    use libc::{CLOCK_MONOTONIC, clock_gettime, timespec};
    unsafe {
        let mut ts = timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        clock_gettime(CLOCK_MONOTONIC, &mut ts);
        (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64)
    }
}

pub struct Trials<T> {
    trials: Vec<T>,
}

impl<T> Trials<T>
where
    T: std::cmp::Ord + std::fmt::Display,
{
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            trials: Vec::with_capacity(capacity),
        }
    }

    pub fn push(&mut self, value: T) {
        self.trials.push(value);
    }

    pub fn len(&self) -> usize {
        self.trials.len()
    }

    pub fn is_empty(&self) -> bool {
        self.trials.len() == 0
    }

    pub fn sort(&mut self) {
        self.trials.sort();
    }

    pub fn min(&self) -> &T {
        self.trials.first().unwrap()
    }

    pub fn max(&self) -> &T {
        self.trials.last().unwrap()
    }

    pub fn quantile(&self, p: f64) -> &T {
        let n = self.trials.len();
        assert!(n > 0);
        assert!((0.0..=1.0).contains(&p));
        let idx = ((n - 1) as f64 * p).round() as usize;
        &self.trials[idx]
    }

    pub fn print_csv(&self, title: &str) {
        println!("name,n,min,max,0.1,0.5,0.75,0.9,0.95,0.99,0.999,0.9999,0.99999");
        println!(
            "{},{},{},{},{},{},{},{},{},{},{},{},{}",
            title,
            self.len(),
            self.min(),
            self.max(),
            self.quantile(0.1),
            self.quantile(0.5),
            self.quantile(0.75),
            self.quantile(0.9),
            self.quantile(0.95),
            self.quantile(0.99),
            self.quantile(0.999),
            self.quantile(0.9999),
            self.quantile(0.99999),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- mono_time_ns tests -------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn mono_time_ns_returns_positive() {
        let t = mono_time_ns();
        assert!(t > 0, "mono_time_ns should return a positive timestamp");
    }

    #[cfg(unix)]
    #[test]
    fn mono_time_ns_is_monotonic_non_decreasing() {
        // Not strictly guaranteed to be strictly increasing for back-to-back calls,
        // but it should not go backwards.
        let t1 = mono_time_ns();
        let t2 = mono_time_ns();
        assert!(
            t2 >= t1,
            "mono_time_ns should be monotonic: t2={} < t1={}",
            t2,
            t1
        );
    }

    #[cfg(unix)]
    #[test]
    fn mono_time_ns_increases_over_sleep() {
        use std::thread;
        use std::time::Duration;

        let t1 = mono_time_ns();
        thread::sleep(Duration::from_millis(5));
        let t2 = mono_time_ns();

        assert!(
            t2 > t1,
            "mono_time_ns should increase over time: t2={} <= t1={}",
            t2,
            t1
        );
    }

    // --- Trials tests -------------------------------------------------------

    #[test]
    fn trials_push_len_min_max_quantile() {
        let mut trials = Trials::with_capacity(10);
        assert_eq!(trials.len(), 0);

        trials.push(5);
        trials.push(1);
        trials.push(9);
        trials.push(3);
        trials.push(7);

        assert_eq!(trials.len(), 5);

        // Sort before using min/max/quantile
        trials.sort();

        // After sort, trials should be [1, 3, 5, 7, 9]
        assert_eq!(*trials.min(), 1);
        assert_eq!(*trials.max(), 9);

        // Check a few quantiles with known positions:
        // n = 5, indices are round((n-1) * p)
        // p = 0.0  -> idx = 0  -> 1
        // p = 0.5  -> idx = 2  -> 5
        // p = 1.0  -> idx = 4  -> 9
        assert_eq!(*trials.quantile(0.0), 1);
        assert_eq!(*trials.quantile(0.5), 5);
        assert_eq!(*trials.quantile(1.0), 9);
    }

    #[test]
    #[should_panic(expected = "n > 0")]
    fn trials_quantile_panics_on_empty() {
        let trials: Trials<u64> = Trials::with_capacity(0);
        // This should panic because len() == 0
        let _ = trials.quantile(0.5);
    }

    #[test]
    #[should_panic]
    fn trials_quantile_panics_on_p_below_zero() {
        let mut trials = Trials::with_capacity(1);
        trials.push(42);
        trials.sort();
        let _ = trials.quantile(-0.1);
    }

    #[test]
    #[should_panic]
    fn trials_quantile_panics_on_p_above_one() {
        let mut trials = Trials::with_capacity(1);
        trials.push(42);
        trials.sort();
        let _ = trials.quantile(1.1);
    }

    #[test]
    fn trials_print_csv_smoke_test() {
        let mut trials = Trials::with_capacity(5);
        trials.push(10);
        trials.push(20);
        trials.push(30);
        trials.sort();

        // Just make sure it doesn't panic.
        // We don't assert on stdout; this is a smoke test.
        trials.print_csv("test_trials");
    }
}
