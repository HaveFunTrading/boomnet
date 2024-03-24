//! Collection of idle strategies.

use std::hint;
use std::time::Duration;

#[derive(Debug, Copy, Clone)]
pub enum IdleStrategy {
    NoOp,
    BusySpin,
    Sleep(Duration),
}

impl IdleStrategy {
    #[inline]
    pub fn idle(&self, work_count: usize) {
        match *self {
            IdleStrategy::NoOp => {}
            IdleStrategy::BusySpin => {
                if work_count == 0 {
                    hint::spin_loop()
                }
            }
            IdleStrategy::Sleep(duration) => {
                if work_count == 0 {
                    std::thread::sleep(duration)
                }
            }
        }
    }
}
