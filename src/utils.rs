/// Returns timestamp in ns
#[cfg(unix)]
#[inline(always)]
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
