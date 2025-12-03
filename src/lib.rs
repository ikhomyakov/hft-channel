//! # SPMC broadcast channel for HFT and real-time systems
//!
//! A lightweight, ultra-low-latency **single-producer / multi-consumer**
//! broadcast channel designed for **high-frequency trading (HFT)** and other
//! real-time systems.
//!
//! Provides predictable performance, minimal contention, and carefully
//! controlled memory access patterns. Supports **thread-to-thread broadcasting**
//! via local memory, or **inter-process communication** using shared memory.
//!
//! # Features
//!
//! * **Lock-free** single-producer / multi-consumer broadcast channel  
//! * **Seq_no protocol** for overwrite detection  
//! * **Spin-wait receivers** for ultra-low latency  
//! * **Cache-friendly** layout (CachePadded)  
//! * Works **between threads** or **between processes** via shared memory  
//! * Zero allocations after initialization  
//! * `no_std`-friendly design  
//!
//! # Spin-Wait Behavior
//!
//! Receivers use **busy-waiting** (spin loops) to wait for the producer to
//! complete publishing the next message.
//!
//! **Implications:**
//!
//! * **Lowest possible latency** (no OS scheduling)  
//! * A receiver consumes **one logical CPU core** while waiting  
//! * Best when producer and receivers are on the **same NUMA node**  
//! * Not ideal when power efficiency is important  
//! * No OS blocking (by design)  
//!
//! This design is intended for **HFT**, **trading engines**, **matching
//! engines**, **real-time telemetry**, and other performance-critical
//! workloads.
//!
//! # Quick Example
//!
//! ```ignore
//! use hft_channel::spmc_broadcast::channel;
//!
//! let (tx, rx) = channel::<u64>("/test", 512);
//!
//! let (_, payload) = tx.reserve();
//! *payload = 42;
//! tx.commit();
//!
//! let (_, payload) = rx.recv();
//! ```
//!
//! # Design Overview
//!
//! Each slot’s state is encoded as:
//!
//! ```text
//! [ dirty (1 bit, MSB) | seq_no (63 bits) ]
//! ```
//!
//! Guarantees:
//!
//! * At least **one slot is always dirty**  
//! * Dirty slot = currently being written  
//! * Clean slot = readable and stable  
//! * If a receiver observes a changed `seq_no`, the slot was overwritten  
//!
//! Typical protocol:
//!
//! 1. Sender marks the **next reserved** slot dirty  
//! 2. Sender writes the payload into the **current reserved** slot  
//! 3. Sender clears the dirty flag (commit)  
//! 4. Receiver waits for `dirty == false`, then reads or copies the payload  

#[cfg(not(unix))]
compile_error!("This crate only supports Unix-like operating systems.");

mod mmap;
pub mod spmc_broadcast;
mod utils;

pub(crate) use mmap::{map_shared_memory, unmap_shared_memory};
pub use utils::{Trials, mono_time_ns};
