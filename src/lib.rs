//! # Ring Buffer
//!
//! A lightweight, high-performance ring buffer designed for use in
//! high-frequency trading (HFT) and other low-latency systems.
//!
//! This crate provides a single-producer, multi-receiver channel
//! implementation with predictable performance characteristics,
//! minimal contention, and carefully controlled memory access patterns.
//!
//! The ring buffer may be used for **intra-thread communication**
//! (broadcasting messages between threads using local memory) or for
//! **inter-process communication** when the backing memory region is
//! placed in shared memory. In both cases, the design avoids locks
//! and minimizes cache-line contention, making it suitable for
//! latency-sensitive workloads.
//!
//! ## Spin-Wait Behavior
//!
//! Please note that **receivers use spin-waiting** while observing the
//! producer’s progress. A receiver continuously polls the `dirty` flag
//! of its current slot until the writer commits the message.
//!
//! This design has several important implications:
//!
//! - **Ultra-low latency:**  
//!   Spin-waiting avoids kernel scheduling and wake-up delays, making
//!   message delivery effectively bounded by memory latency rather than
//!   OS overhead.
//!
//! - **CPU usage:**  
//!   A waiting receiver **consumes a full logical core** while spinning.
//!   This is typically acceptable (and desirable) in HFT or real-time
//!   systems, but it may not be suitable for general-purpose workloads.
//!
//! - **NUMA/cache sensitivity:**  
//!   Because receivers poll a shared cache line, performance is best
//!   when threads are placed on the same NUMA node as the producer.
//!
//! - **No blocking primitives:**  
//!   The implementation never parks threads or uses OS synchronization
//!   primitives. This makes it ideal for low-latency pipelines, but
//!   not for applications requiring power efficiency or fairness.
//!
//! ## Modules
//!
//! - [`ring_buffer`] — Core types (`Sender`, `Receiver`, `Message`) implementing
//!   the lock-free ring-buffer protocol.
//! - [`utils`] — Benchmarking and experimentation helpers.
//!
//! ## License
//!
//! Copyright © 2005–2025  
//! IKH Software, Inc.
//!
//! Licensed under the terms of the **GNU Lesser General Public License**,  
//! version 3.0, or (at your option) any later version.  
//!
//! See <https://www.gnu.org/licenses/lgpl-3.0.html> for details.

#[cfg(not(unix))]
compile_error!("This crate only supports Unix-like operating systems.");

mod ring_buffer;
mod utils;

pub use ring_buffer::{Message, Receiver, Sender};
pub use utils::{mono_time_ns, Trials};
