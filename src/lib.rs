//! # Ring buffer
//!
//! A lightweight ring buffer for HFT.
//!
//! ## License
//!
//! Copyright (c) 2005–2025 IKH Software, Inc.
//!
//! Released under the terms of the GNU Lesser General Public License, version 3.0 or
//! (at your option) any later version (LGPL-3.0-or-later).

mod buffer;
mod trials;

pub use buffer::{Receiver, Sender, Message};
pub use trials::{Trials};

pub const PAYLOAD_SIZE: usize = 248;

pub const BUFFER_LEN: usize = 4096;
const _: () = assert!(BUFFER_LEN.is_power_of_two() && BUFFER_LEN > 1);

/// Example payload type used in each ring-buffer slot (wrapped in a `Message`).
#[derive(Clone, Debug)]
#[repr(C)]
pub struct Payload<const N: usize> {
    pub timestamp: u64,
    pub bytes: [u8; N],
}

impl<const N: usize> Default for Payload<N> {
    fn default() -> Self {
        Self {
            timestamp: 0,
            bytes: [0u8; N],
        }
    }
}

pub const BUFFER_SIZE: usize = BUFFER_LEN * std::mem::size_of::<Message<Payload<PAYLOAD_SIZE>>>();