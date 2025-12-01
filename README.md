# SPMC broadcast channel for HFT and real-time systems

A lightweight, ultra-low-latency **single-producer / multi-consumer** broadcast channel designed for use in **high-frequency trading (HFT)** and other real-time systems.

Provides predictable performance, minimal contention, and carefully controlled memory access patterns.
Works for **thread-to-thread broadcasting** via local memory, or **inter-process communication** using shared memory.

[![Crates.io](https://img.shields.io/crates/v/hft-channel.svg)](https://crates.io/crates/hft-channel)
[![Documentation](https://docs.rs/ichannel/badge.svg)](https://docs.rs/hft-channel)
[![License: LGPL-3.0-or-later](https://img.shields.io/badge/License-LGPL%203.0--or--later-blue.svg)](https://www.gnu.org/licenses/lgpl-3.0)
[![Rust](https://img.shields.io/badge/rust-stable-brightgreen.svg)](https://www.rust-lang.org)

## Features

* **Lock-free** single-producer / multi-consumer broadcast channel
* **Seq_no protocol** ensures overwrite detection
* **Spin-wait receivers** for ultra-low latency
* **Cache-friendly** layout (CachePadded)
* Works **between threads**, or **between processes** via shared memory
* Zero allocations after initialization
* `no_std`-friendly design

## Spin-Wait Behavior

Receivers use **busy-waiting** (spin loops) to wait for the producer to
complete publishing the next message.

**Implications:**

* **Lowest possible latency** (no OS scheduling)
* A receiver consumes **one logical CPU core** while waiting
* Best when producer and receivers are on the **same NUMA node**
* Not ideal for scenarios where power efficiency matters
* No OS blocking (by design)

This design is intended for **HFT**, **trading engines**, **matching engines**, **real-time telemetry**, and other performance-critical workloads.

## Installation

```toml
[dependencies]
hft-channel = "0.1"
```

## Example

```rust
use hft-channel::spmc_broadcast::channel;

let (tx, rx) = channel::<u64>("/test", 512);

let (_, payload) = tx.reserve();
*payload = 42;
tx.commit();

let (_, payload) = rx.recv();
```

## Design Overview

The channel tracks state as:

```
[ dirty (1 bit, MSB) | seq_no (63 bits) ]
```

Guarantees:

* At least **one slot is always dirty**
* Dirty slot = being written, reserved for writing
* Clean slot = readable and stable
* If receiver sees *seq_no changed*, the slot was overwritten

Pattern:

1. Sender marks **next reserved** slot dirty
2. Sender writes payload into **current reserved** slot
3. Sender clears dirty flag of current slot (= commit)
4. Receiver waits for `dirty == false`, then copies or peeks

## Benchmarks

Run the provided micro-benchmarks:

```bash
cargo bench
```

Or see examples in `examples/`.

## Testing

```bash
cargo test
```

## License

Copyright © 2005–2025 IKH Software, Inc.

Licensed under **LGPL-3.0-or-later**.
See [`LICENSE`](LICENSE) or [https://www.gnu.org/licenses/lgpl-3.0.html](https://www.gnu.org/licenses/lgpl-3.0.html).

## Contributing

Contributions are welcome! Please open issues or pull requests on GitHub.

By submitting a contribution, you agree that it will be licensed under the
project’s **LGPL-3.0-or-later** license.

