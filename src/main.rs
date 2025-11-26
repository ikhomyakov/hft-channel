use std::env;
use std::io;
use std::ptr;
use std::slice;
//use std::io::Write;

/// Returns timestamp in CPU cycles
#[inline(always)]
fn rdtscp() -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        core::arch::asm!(
            "rdtscp",
            //"lfense",
            out("eax") low,
            out("edx") high,
            out("ecx") _,
            options(nomem, nostack, preserves_flags),
        );
    }
    ((high as u64) << 32) | (low as u64)
}

const N_RECEIVERS: usize = 5;
const KEY: libc::key_t = 0x1234;

const PAYLOAD_SIZE: usize = 200;
const BUFFER_LEN: usize = 4096;
const _: () = assert!(BUFFER_LEN.is_power_of_two() && BUFFER_LEN > 1);
const BUFFER_SIZE: usize = BUFFER_LEN * std::mem::size_of::<Message<Payload<PAYLOAD_SIZE>>>();

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} writer|reader|both", args[0]);
        std::process::exit(1);
    }

    match args[1].as_str() {
        "writer" => writer(),
        "reader" => reader(),
        "both" => {
            let mut receivers = Vec::new();
            for _ in 0..N_RECEIVERS {
                receivers.push(std::thread::spawn(reader));
            }
            let sender = std::thread::spawn(writer);
            for _ in 0..N_RECEIVERS {
                receivers.pop().unwrap().join().unwrap()?;
            }
            sender.join().unwrap()?;
            Ok(())
        }
        _ => {
            eprintln!("Usage: {} writer|reader|both", args[0]);
            std::process::exit(1);
        }
    }
}

fn writer() -> io::Result<()> {
    let shmid = unsafe { libc::shmget(KEY, BUFFER_SIZE, libc::IPC_CREAT | 0o600) };
    if shmid == -1 {
        return Err(io::Error::last_os_error());
    }

    let addr = unsafe { libc::shmat(shmid, ptr::null(), 0) };
    if addr == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }

    println!("writer: attached at {:p}, shmid = {}", addr, shmid);

    let buf = unsafe {
        slice::from_raw_parts_mut(addr as *mut Message<Payload<PAYLOAD_SIZE>>, BUFFER_LEN)
    };
    println!("buf.len() = {}", buf.len());
    // let x = [1, 2, 3];
    // println!("{:?}", x.as_ptr() as *const u8);

    const TRIALS: usize = 100_000;
    let mut trials = Trials::with_capacity(TRIALS);
    let mut trials2 = Trials::with_capacity(TRIALS);

    let mut payload = Payload::<PAYLOAD_SIZE>::default();

    let mut tx = Sender::new(buf);

    loop {
        let ts0 = rdtscp();
        payload.timestamp = ts0;
        tx.send(ts0, &payload);
        let ts1 = rdtscp();
        let ts2 = rdtscp();
        trials.push(ts1 - ts0);
        trials2.push(ts2 - ts1);
        if trials.len() == TRIALS {
            break;
        }
    }

    trials.sort();
    trials2.sort();
    trials.print_csv("A");
    trials2.print_csv("X");

    Ok(())
}

fn reader() -> io::Result<()> {
    let shmid = unsafe { libc::shmget(KEY, BUFFER_SIZE, 0o600) };
    if shmid == -1 {
        return Err(io::Error::last_os_error());
    }

    let addr = unsafe { libc::shmat(shmid, ptr::null(), 0) };
    if addr == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }

    println!("reader: attached at {:p}, shmid = {}", addr, shmid);

    let buf =
        unsafe { slice::from_raw_parts(addr as *const Message<Payload<PAYLOAD_SIZE>>, BUFFER_LEN) };
    println!("buf.len() = {}", buf.len());

    const TRIALS: usize = 100_000;
    let mut trials = Trials::with_capacity(TRIALS);
    let mut trials2 = Trials::with_capacity(TRIALS);

    let mut rx = Receiver::new(buf);

    let mut prev_seq_no: u64 = 0;
    loop {
        // print!("{}      \r", trials.len());
        // io::stdout().flush().unwrap();
        let ts0 = rdtscp();

        let (seq_no, ts1) = if false {
            // This performs a full payload copy.
            let (seq_no, user_state, payload) = rx.recv();
            debug_assert!(user_state == payload.timestamp);
            (seq_no, payload.timestamp) // payload reference is stable
        } else if true {
            // This avoids copying the payload by peeking directly into the slot.
            // Prevents payload-related concurrency issues by not accessing the
            // payload. Relies solely on user_state.
            let (seq_no, user_state) = rx.load_state();
            rx.advance();
            (seq_no, user_state)
        } else {
            // This avoids copying the payload by peeking directly into the slot.
            // `peek_unsafe` returns a reference into the ring buffer, and `advance_checked`
            // confirms that the slot was not overwritten while reading it.
            loop {
                let (seq_no, user_state, payload) = unsafe { rx.peek_unsafe() };
                let ts1 = payload.timestamp;
                if rx.advance_checked() {
                    debug_assert!(user_state == ts1);
                    break (seq_no, ts1);
                }
            }
        };

        let ts2 = rdtscp();
        trials.push(ts2 - ts1);
        trials2.push(ts2 - ts0);
        if prev_seq_no != 0 && prev_seq_no.wrapping_add(1) != seq_no {
            println!(
                "Skipped {} messages: prev seq_no {}, curr seq_no {}",
                seq_no - prev_seq_no,
                prev_seq_no,
                seq_no
            );
        }
        prev_seq_no = seq_no;
        if trials.len() == TRIALS {
            break;
        }
    }

    trials.sort();
    trials2.sort();
    trials.print_csv("B");
    trials2.print_csv("C");

    Ok(())
}

use crossbeam_utils::CachePadded;
use portable_atomic::{AtomicU128, Ordering};
use std::fmt::Debug;

/// Packed state containing the dirty flag, sequence number and user state.
///
/// Stored as a single `u128` inside an `AtomicU128`:
///
/// Layout (most-significant bit first):
/// 127:      dirty flag (bool)
/// 64–126:   seq_no (u64, 63 bits used)
/// 0–63:     user_state (u64, all 64 bits)
///
/// # Meaning of states
///
/// - **dirty = false, seq_no = 0**
///   Slot is empty and has never been written.
///
/// - **dirty = false, seq_no > 0**
///   Slot holds a fully committed message.
///
/// - **dirty = true, seq_no = 0**
///   **Unreachable state.**
///   A slot cannot be dirty without an associated (non-zero) `seq_no`.
///
/// - **dirty = true,  seq_no > 0**
///   Slot is currently being written with a message identified by `seq_no`.
#[derive(Default)]
struct State(AtomicU128);

const DIRTY_BIT: u32 = 127;
const DIRTY_MASK: u128 = 1u128 << DIRTY_BIT;

const SEQ_SHIFT: u32 = 64;
const SEQ_BITS: u32 = 63;
const SEQ_MASK: u128 = ((1u128 << SEQ_BITS) - 1) << SEQ_SHIFT;

const USER_MASK: u128 = (1u128 << 64) - 1;

impl State {
    /// Creates a new packed state from `dirty`, `seq_no`, and `user_state`.
    ///
    /// # Panics (debug only)
    ///
    /// `seq_no` must fit in 63 bits; in debug builds this is checked
    /// with a `debug_assert!`.
    fn new(dirty: bool, seq_no: u64, user_state: u64) -> Self {
        debug_assert!(seq_no < (1u64 << SEQ_BITS));

        let dirty_bit = (dirty as u128) << DIRTY_BIT;
        let seq_bits = (seq_no as u128) << SEQ_SHIFT;
        let user_bits = user_state as u128;

        Self(AtomicU128::new(dirty_bit | seq_bits | user_bits))
    }

    /// Loads the current state with the given memory ordering.
    ///
    /// Returns `(dirty, seq_no, user_state)`.
    fn load(&self, order: Ordering) -> (bool, u64, u64) {
        let v = self.0.load(order);

        let dirty = (v & DIRTY_MASK) != 0;
        let seq_no = ((v & SEQ_MASK) >> SEQ_SHIFT) as u64;
        let user_state = (v & USER_MASK) as u64;

        (dirty, seq_no, user_state)
    }

    /// Stores a new `(dirty, seq_no, user_state)` triple with the given memory ordering.
    fn store(&self, dirty: bool, seq_no: u64, user_state: u64, order: Ordering) {
        debug_assert!(seq_no < (1u64 << SEQ_BITS));

        let dirty_bits = (dirty as u128) << DIRTY_BIT;
        let seq_bits = (seq_no as u128) << SEQ_SHIFT;
        let user_bits = user_state as u128;

        let v = dirty_bits | seq_bits | user_bits;
        self.0.store(v, order);
    }
}

impl Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (dirty, seq_no, user_state) = self.load(Ordering::SeqCst);
        f.debug_struct("State")
            .field("dirty", &dirty)
            .field("seq_no", &seq_no)
            .field("user_state", &user_state)
            .finish()
    }
}

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

/// Internal ring-buffer slot.
///
/// Each `Message` consists of a `State` (dirty flag + sequence number)
/// and a `payload`. Both fields are cache padded to reduce false sharing
/// between producer and consumers.
#[derive(Debug, Default)]
#[repr(C)]
pub struct Message<T: Sized> {
    state: CachePadded<State>,
    payload: CachePadded<T>,
}

/// Single-producer sender for a ring buffer.
///
/// This channel provides **no backpressure**: when the buffer becomes full,
/// sending a new message will **overwrite the oldest message**. It is the
/// receiver’s responsibility to keep up with the sender if message loss is
/// unacceptable. The receiver can detect message loss by observing gaps in
/// the monotonically increasing `seq_no`.
///
/// Receivers must only read the payload from slots that are **not dirty**,
/// and after consuming the payload they must verify that the `seq_no` is
/// unchanged. If `seq_no` changed, the slot was overwritten and the payload
/// must be discarded.
///
/// # Protocol
///
/// The sender always keeps **one dirty slot**, which is the slot at `position`.
/// Each publish performs:
///
/// 1. **Write** the message into the dirty slot at `position`.
/// 2. **Dirty next slot** (`position + 1`), making it the next write target.
/// 3. **Clear dirty on current slot**, signaling the write is complete.
///
/// This keeps at least one dirty slot at all times, allowing receivers to
/// detect progress and catch up by scanning forward until they find a dirty
/// slot, then waiting for dirty transitions.
///
/// # Fast wrap-around
///
/// If the sender wraps and overwrites a slot while a receiver is reading it,
/// the receiver can use the `seq_no` to detect this: after reading the payload,
/// it can check whether `seq_no` is unchanged. If it changed, the slot was
/// overwritten and the receiver must discard the payload.
#[derive(Debug)]
pub struct Sender<'a, T> {
    position: usize,
    seq_no: u64,
    buffer: &'a mut [Message<T>],
}

impl<'a, T: Clone + Default> Sender<'a, T> {
    /// Constructs a sender from an existing ring buffer.
    ///
    /// If the buffer already contains a dirty slot, this picks up from the
    /// position and `seq_no` that the previous sender left off at.
    ///
    /// If no dirty slot is found, this falls back to [`Sender::with_init`]
    /// and initializes the entire ring buffer.
    pub fn new(buffer: &'a mut [Message<T>]) -> Self {
        if let Some(position) = buffer.iter().position(|x| x.state.load(Ordering::SeqCst).0) {
            let (_, seq_no, _) = buffer[position].state.load(Ordering::SeqCst);
            Self {
                position,
                seq_no,
                buffer,
            }
        } else {
            Self::with_init(buffer)
        }
    }

    /// Constructs a sender and unconditionally initializes the ring buffer.
    ///
    /// Slot `0` is initialized as:
    ///
    /// - `dirty = true`
    /// - `seq_no = 1`
    /// - `user_state = 0`
    ///
    /// All other slots are initialized as:
    ///
    /// - `dirty = false`
    /// - `seq_no = 0`
    /// - `user_state = 0`
    ///
    /// This forms a valid starting state with a single dirty slot.
    pub fn with_init(buffer: &'a mut [Message<T>]) -> Self {
        buffer[0] = Message {
            state: CachePadded::new(State::new(true, 1, 0)),
            payload: CachePadded::new(T::default()),
        };
        for i in 1..BUFFER_LEN {
            buffer[i] = Message {
                state: CachePadded::new(State::new(false, 0, 0)),
                payload: CachePadded::new(T::default()),
            };
        }
        Self {
            position: 0,
            seq_no: 1,
            buffer,
        }
    }

    /// Sends a message into the ring buffer.
    ///
    /// The sender marks the next slot as dirty with the next `seq_no`, writes
    /// the payload into the current slot, and commits the current slot by clearing
    /// its dirty flag. This preserves the invariant that there is always at least
    /// one dirty slot in the ring. The method returns the `seq_no` for the current
    /// message, and afterward `self.position` points to the newly dirtied slot.
    #[inline(always)]
    pub fn send(&mut self, user_state: u64, payload: &T) -> u64 {
        let next_position = self.position.wrapping_add(1) % BUFFER_LEN;

        let seq_no = self.seq_no;
        let next_seq_no = seq_no.wrapping_add(1);

        let next_slot = &mut self.buffer[next_position];
        next_slot
            .state
            .store(true, next_seq_no, 0, Ordering::SeqCst);

        let slot = &mut self.buffer[self.position];
        (*slot.payload).clone_from(payload);
        slot.state
            .store(false, seq_no, user_state, Ordering::SeqCst);

        self.position = next_position;
        self.seq_no = next_seq_no;
        seq_no
    }
}

/// Multi-receiver handle for reading from a ring buffer.
///
/// A `Receiver`:
///
/// - does not mutate the underlying ring buffer’s structure,
/// - tracks its own read position and expected `seq_no`, and
/// - maintains a single temporary slot (`recv_slot`) used to return
///   stable references to payloads.
///
/// Each receiver can lag behind of the sender. If a receiver
/// falls behind and its target slot is overwritten, it can detect this
/// via the `seq_no` check.
#[derive(Debug)]
pub struct Receiver<'a, T> {
    position: usize, // points to `dirty` slot, i.e. the slot being written
    seq_no: u64,     // current `seq_no`, i.e. the `seq_no` expected to be written
    buffer: &'a [Message<T>],
    recv_slot: T,
}

impl<'a, T: Clone + Default + Debug> Receiver<'a, T> {
    /// Constructs a receiver starting from the current dirty slot.
    ///
    /// This scans the buffer for a dirty slot (the one being written) and
    /// initializes the receiver’s `position` and `seq_no` accordingly.
    ///
    /// # Panics
    ///
    /// Panics if no dirty slot is found, which indicates an uninitialized
    /// or corrupted buffer.
    pub fn new(buffer: &'a [Message<T>]) -> Self {
        if let Some(position) = buffer.iter().position(|x| x.state.load(Ordering::SeqCst).0) {
            let (_, seq_no, _) = buffer[position].state.load(Ordering::SeqCst);
            Self {
                position,
                seq_no,
                buffer,
                recv_slot: T::default(),
            }
        } else {
            panic!("Uninitialized buffer!");
        }
    }

    /// Blocking receive.
    ///
    /// Waits until the current readable slot is fully committed, copies
    /// its payload into the internal receiver's area, and then verifies that
    /// the slot has not been overwritten during the copy.
    ///
    /// On success, advances this receiver to the next message and returns:
    ///
    /// - the `seq_no` of the received message, and
    /// - a stable reference to the payload.
    ///
    /// This method will spin-wait if the slot is still dirty.
    #[inline(always)]
    pub fn recv(&mut self) -> (u64, u64, &T) {
        loop {
            let (seq_no, user_state) = self.load_state();
            let slot = &self.buffer[self.position];
            self.recv_slot.clone_from(&slot.payload);
            let (_, seq_no2, _) = self.try_load_state();
            if seq_no2 == seq_no {
                self.advance();
                return (seq_no, user_state, &self.recv_slot);
            }
        }
    }

    /// Non-blocking receive.
    ///
    /// Attempts to read a payload from the current slot.
    ///
    /// Returns:
    ///
    /// - `(seq_no, user_state, Some(&T))` on success, after advancing to the next slot.
    /// - `(seq_no, user_state, None)` if the slot is dirty or if it was overwritten while
    ///   copying (detected by a changed `seq_no`). In this case, the receiver
    ///   does **not** advance to the next slot.
    #[inline(always)]
    pub fn try_recv(&mut self) -> (u64, u64, Option<&T>) {
        let (dirty, seq_no, user_state) = self.try_load_state();
        if !dirty {
            let slot = &self.buffer[self.position];
            self.recv_slot.clone_from(&(*slot.payload));
            let (_, seq_no2, user_state2) = self.try_load_state();
            if seq_no2 == seq_no {
                self.advance();
                (seq_no, user_state, Some(&self.recv_slot))
            } else {
                (seq_no2, user_state2, None)
            }
        } else {
            (seq_no, user_state, None)
        }
    }

    /// Unsafely receives the next slot without copying the payload.
    ///
    /// Unlike a safe `recv`, this method returns a direct reference to the slot’s
    /// payload inside the ring buffer. It also returns the associated `seq_no` and
    /// `user_state`, which are the primary reasons to call this method.
    ///
    /// Because this operation *immediately advances* the receiver’s position,
    /// there is **no way to verify** that the returned slot has not already been
    /// overwritten by the sender. As a result, the returned reference may become
    /// invalid at any time after this call.
    ///
    /// This is useful only in scenarios where avoiding a payload copy is critical
    /// and the caller can guarantee that the slot will not be reused before the
    /// reference is fully consumed, or when payload is not used at all.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the returned reference is **not used after the
    /// underlying slot may have been overwritten** by the sender. Since this
    /// method advances the receiver immediately, you cannot detect races the way
    /// `peek_unsafe` + `advance_checked()` allows.
    #[inline(always)]
    pub unsafe fn recv_unsafe(&mut self) -> (u64, u64, &T) {
        let (seq_no, user_state) = self.load_state();
        self.advance();
        let slot = &self.buffer[self.position];
        (seq_no, user_state, &slot.payload)
    }

    /// Non-blocking version of [`Receiver::recv_unsafe`].
    ///
    /// Returns:
    ///
    /// - `(seq_no, user_state, Some(&T))` if the slot is currently clean (committed).
    /// - `(seq_no, user_state, None)` if the slot is dirty (still being written).
    ///
    /// # Safety
    ///
    /// Same caveats as [`Receiver::recv_unsafe`]: the returned reference
    /// may be invalidated if the slot is overwritten.
    #[inline(always)]
    pub unsafe fn try_recv_unsafe(&mut self) -> (u64, u64, Option<&T>) {
        let (dirty, seq_no, user_state) = self.try_load_state();
        let slot = &self.buffer[self.position];
        if !dirty {
            self.advance();
            (seq_no, user_state, Some(&slot.payload))
        } else {
            (seq_no, user_state, None)
        }
    }

    /// Unsafely peeks at the current readable slot without copying.
    ///
    /// This returns a reference directly into the ring buffer, rather than
    /// copying into receiver's area. Because the slot can be overwritten by the
    /// sender, the returned reference is only valid as long as the slot is
    /// not overwritten.
    ///
    /// Use this together with [`Receiver::advance_checked`] to validate the peek:
    ///
    /// ```ignore
    /// loop {
    ///     let (seq_no, user_state, payload) = rx.peek_unsafe();
    ///     // ... use `payload` speculatively ...
    ///     if rx.advance_checked() {
    ///         // `payload` was not overwritten; work is valid.
    ///         break;
    ///     } else {
    ///         // Slot may have been overwritten; roll back and retry.
    ///     }
    /// }
    /// ```
    ///
    /// # Safety
    ///
    /// The caller must ensure that the reference is not used after the slot
    /// has potentially been overwritten. In practice, you must call
    /// [`Receiver::advance_checked`] and check its return value before relying on
    /// any work derived from `payload`.
    #[inline(always)]
    pub unsafe fn peek_unsafe(&self) -> (u64, u64, &T) {
        let (seq_no, user_state) = self.load_state();
        let slot = &self.buffer[self.position];
        (seq_no, user_state, &slot.payload)
    }

    /// Non-blocking version of [`Receiver::peek_unsafe`].
    ///
    /// Returns:
    ///
    /// - `(seq_no, user_state, Some(&T))` if the slot is currently clean (committed).
    /// - `(seq_no, user_state, None)` if the slot is dirty (still being written).
    ///
    /// # Safety
    ///
    /// Same caveats as [`Receiver::peek_unsafe`]: the returned reference
    /// may be invalidated if the slot is overwritten, and must be validated
    /// with [`Receiver::advance_checked`].
    #[inline(always)]
    pub unsafe fn try_peek_unsafe(&self) -> (u64, u64, Option<&T>) {
        let (dirty, seq_no, user_state) = self.try_load_state();
        let slot = &self.buffer[self.position];
        if !dirty {
            (seq_no, user_state, Some(&slot.payload))
        } else {
            (seq_no, user_state, None)
        }
    }

    /// Attempts to advance the receiver past the current slot.
    ///
    /// This is intended to be used after a speculative peek (see
    /// [`Receiver::peek_unsafe`]). It re-reads the state of the current slot
    /// and checks whether the `seq_no` still matches `self.seq_no`.
    ///
    /// - If the `seq_no` matches, the slot has not been overwritten:
    ///   - the receiver advances to the next slot, and
    ///   - the method returns `true`.
    ///
    /// - If the `seq_no` differs, the slot was overwritten:
    ///   - the receiver does **not** advance, and
    ///   - the method returns `false`.
    #[inline(always)]
    pub fn advance_checked(&mut self) -> bool {
        let (_, seq_no, _) = self.try_load_state();
        let valid = self.seq_no == seq_no;
        self.position = self.position.wrapping_add(valid as usize) % BUFFER_LEN;
        self.seq_no = seq_no.wrapping_add(valid as u64);
        valid
    }

    /// Advances the receiver past the current slot unconditionally.
    ///
    /// This is intended to be used after [`Receiver::load_state`] or.
    /// [`Receiver::try_load_state`]).
    #[inline(always)]
    pub fn advance(&mut self) {
        self.position = self.position.wrapping_add(1) % BUFFER_LEN;
        self.seq_no = self.seq_no.wrapping_add(1);
    }

    /// Spin-waits until the current slot becomes clean (committed).
    ///
    /// This loops while the dirty flag is set, repeatedly loading the state.
    /// Once the slot is clean, it returns the observed `seq_no` and `user_state`.
    #[inline(always)]
    pub fn load_state(&self) -> (u64, u64) {
        // Spin-wait on `dirty` flag
        let slot = &self.buffer[self.position];
        let (seq_no, user_state) = loop {
            let (dirty, seq_no, user_state) = slot.state.load(Ordering::SeqCst);
            if !dirty {
                break (seq_no, user_state);
            }
            // core::hint::spin_loop(); // Consider adding it. It adds asm instruction `pause`.
        };
        (seq_no, user_state)
    }

    /// Returns the current slot’s `(dirty, seq_no, user_state)` without blocking.
    ///
    /// This method performs a single atomic load of the slot state at the
    /// receiver's current position. It does **not** spin-wait for the slot to
    /// become clean; callers must handle the case where `dirty == true`.
    ///
    /// This is the non-blocking counterpart of [`Receiver::load_state`].
    #[inline(always)]
    pub fn try_load_state(&self) -> (bool, u64, u64) {
        let slot = &self.buffer[self.position];
        slot.state.load(Ordering::SeqCst)
    }
}

struct Trials<T> {
    trials: Vec<T>,
}

impl<T> Trials<T>
where
    T: std::cmp::Ord + std::fmt::Display,
{
    fn with_capacity(capacity: usize) -> Self {
        Self {
            trials: Vec::with_capacity(capacity),
        }
    }

    fn push(&mut self, value: T) {
        self.trials.push(value);
    }

    fn len(&self) -> usize {
        self.trials.len()
    }

    fn sort(&mut self) {
        self.trials.sort();
    }

    fn min(&self) -> &T {
        self.trials.first().unwrap()
    }

    fn max(&self) -> &T {
        self.trials.last().unwrap()
    }

    fn quantile(&self, p: f64) -> &T {
        let n = self.trials.len();
        assert!(n > 0);
        assert!((0.0..=1.0).contains(&p));
        let idx = ((n - 1) as f64 * p).round() as usize;
        &self.trials[idx]
    }

    fn print_csv(&self, title: &str) {
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
