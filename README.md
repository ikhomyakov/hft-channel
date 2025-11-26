# **ichannel — High-Performance Ring Buffer for HFT and real-time systems**

A lightweight, ultra-low-latency **single-producer / multi-receiver** ring buffer designed for use in **high-frequency trading (HFT)** and other real-time systems.

Provides predictable performance, minimal contention, and carefully controlled memory access patterns.
Works for **thread-to-thread broadcasting** via local memory, or **inter-process communication** using shared memory.

[![Crates.io](https://img.shields.io/crates/v/ichannel.svg)](https://crates.io/crates/ichannel)
[![Documentation](https://docs.rs/ichannel/badge.svg)](https://docs.rs/ichannel)
[![License: LGPL-3.0-or-later](https://img.shields.io/badge/License-LGPL%203.0--or--later-blue.svg)](https://www.gnu.org/licenses/lgpl-3.0)
[![Rust](https://img.shields.io/badge/rust-stable-brightgreen.svg)](https://www.rust-lang.org)


## ✨ Features

* ⚡ **Lock-free** single-producer / multi-receiver channel
* 🧩 **Dirty + seq_no protocol** ensures overwrite detection
* 🧵 **Spin-wait receivers** for ultra-low latency
* 🧠 **Cache-friendly** layout (CachePadded)
* 🧰 Works **between threads**, or **between processes** via shared memory
* 🔄 Constant-time wrap-around with no syscalls
* 🛡️ Zero allocations after initialization
* 📦 `no_std`-friendly design (*optional*, depending on features)


## ⚠️  Spin-Wait Behavior

Receivers use **busy-waiting** (spin loops) when the producer is writing to their slot.

**Implications:**

* 🚀 **Lowest possible latency** (no OS scheduling)
* 🧮 A receiver consumes **one logical CPU core** while waiting
* 🔁 Best when producer and receivers are on the **same NUMA node**
* 🔋 Not ideal for scenarios where power efficiency matters
* ⛔ No OS blocking (by design)

This design is intended for **HFT**, **trading engines**, **matching engines**, **real-time telemetry**, and other performance-critical workloads.


## 📦 Installation

```toml
[dependencies]
ichannel = "0.1"
```


## 🚀 Quick Example

```rust
use ichannel::{Sender, Receiver, Message};

const N: usize = 1024;

// Initialize ring buffer (shared between threads/processes)
let mut buffer = vec![Message::<u64>::default(); N];

// Construct producer and consumer
let mut tx = Sender::with_init(&mut buffer);
let mut rx = Receiver::new(&buffer);

// Send a message
tx.send(&42);

// Receive a message (spin-waiting)
let (seq, value) = rx.recv();
println!("Got: seq={seq} val={value}");
```


## 🔧 Shared Memory Example (IPC)

You can place the ring buffer inside an mmap’d shared memory region:

```rust
use memmap2::MmapMut;
use ichannel::{Sender, Receiver, Message};

let shm = MmapMut::map_anon(8192 * 1024)?;
let ptr = shm.as_mut_ptr() as *mut Message<u64>;
let buffer = unsafe { std::slice::from_raw_parts_mut(ptr, 1024) };

let mut tx = Sender::with_init(buffer);
// In another process:
let rx = Receiver::new(buffer);
```

The protocol remains correct across processes.


## 🔬 Design Overview

The ring buffer tracks state as:

```
[ dirty (1 bit, MSB) | seq_no (63 bits) ]
```

Guarantees:

* At least **one slot is always dirty**
* Dirty slot = being written
* Clean slot = readable and stable
* If receiver sees *seq_no changed*, the slot was overwritten

Pattern:

1. Sender marks **next** slot dirty (`seq_no+1`)
2. Sender writes payload into **current** slot
3. Sender clears dirty flag of current slot (= commit)
4. Receiver waits for `dirty == false`, then copies or peeks


## 🛡️ Safety & Memory Model

* Atomic state transitions via `AtomicU64`
* No torn reads
* Sequentially consistent protocol
* Receivers guarantee correctness even during overwrites
* Optional “unsafe peek” with version checking for **zero-copy** read paths


## 📊 Benchmarks

Run the provided micro-benchmarks:

```bash
cargo bench
```

Or see examples in `examples/`.


## 🧪 Testing

```bash
cargo test
```


## 📄 License

Copyright © 2005–2025
IKH Software, Inc.

Licensed under **LGPL-3.0-or-later**.
See [`LICENSE`](LICENSE) or [https://www.gnu.org/licenses/lgpl-3.0.html](https://www.gnu.org/licenses/lgpl-3.0.html).

---

## 🤝 Contributing

Contributions are welcome!
Please open issues or pull requests on GitHub.

