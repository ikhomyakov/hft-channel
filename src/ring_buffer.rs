use crossbeam_utils::CachePadded;
use std::fmt::Debug;
use std::sync::atomic::{AtomicU64, Ordering};

/// Packed state containing the dirty flag and sequence number.
///
/// Stored as a single `u64` inside an `AtomicU64`:
///
/// ```text
/// [ dirty (1 bit, MSB) | seq_no (63 bits) ]
///                bit 63        bits 0–62
/// ```
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
///
#[derive(Default)]
struct State(AtomicU64);

const DIRTY_BIT: u64 = 63;
const DIRTY_MASK: u64 = 1 << DIRTY_BIT;

impl State {
    /// Creates a new packed state from a `dirty` flag and a `seq_no`.
    ///
    /// # Panics (debug only)
    ///
    /// `seq_no` must fit in the low 63 bits; in debug builds this is checked
    /// with a `debug_assert!`.
    fn new(dirty: bool, seq_no: u64) -> Self {
        debug_assert!(seq_no < DIRTY_MASK);
        Self(AtomicU64::new(seq_no | ((dirty as u64) << DIRTY_BIT)))
    }

    /// Loads the current state with the given memory ordering.
    ///
    /// Returns `(dirty, seq_no)`.
    #[inline(always)]
    fn load(&self, order: Ordering) -> (bool, u64) {
        let v = self.0.load(order);
        (v & DIRTY_MASK != 0, v & !DIRTY_MASK)
    }

    /// Stores a new `(dirty, seq_no)` pair with the given memory ordering.
    #[inline(always)]
    fn store(&self, dirty: bool, seq_no: u64, order: Ordering) {
        let v = seq_no | ((dirty as u64) << DIRTY_BIT);
        self.0.store(v, order);
    }
}

impl Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (dirty, seq_no) = self.load(Ordering::SeqCst);
        f.debug_struct("State")
            .field("dirty", &dirty)
            .field("seq_no", &seq_no)
            .finish()
    }
}

/// Ring buffer slot.
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
pub struct Sender<'a, T, const BUFFER_LEN: usize> {
    position: usize, // points to `dirty` slot, i.e. the slot being written
    seq_no: u64,     // current `seq_no`, i.e. the `seq_no` of the message being written
    buffer: &'a mut [Message<T>],
}

impl<'a, T: Clone + Default, const BUFFER_LEN: usize> Sender<'a, T, BUFFER_LEN> {
    /// Constructs a sender from an existing ring buffer.
    ///
    /// If the buffer already contains a dirty slot, this picks up from the
    /// position and `seq_no` that the previous sender left off at.
    ///
    /// If no dirty slot is found, this falls back to [`Sender::with_init`]
    /// and initializes the entire ring buffer.
    pub fn new(buffer: &'a mut [Message<T>]) -> Self {
        assert!(BUFFER_LEN.is_power_of_two() && BUFFER_LEN > 1);
        if let Some(position) = buffer.iter().position(|x| x.state.load(Ordering::SeqCst).0) {
            let (_, seq_no) = buffer[position].state.load(Ordering::SeqCst);
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
    ///
    /// All other slots are initialized as:
    ///
    /// - `dirty = false`
    /// - `seq_no = 0`
    ///
    /// This forms a valid starting state with a single dirty slot.
    pub fn with_init(buffer: &'a mut [Message<T>]) -> Self {
        assert!(BUFFER_LEN.is_power_of_two() && BUFFER_LEN > 1);
        buffer[0] = Message {
            state: CachePadded::new(State::new(true, 1)),
            payload: CachePadded::new(T::default()),
        };
        for i in 1..BUFFER_LEN {
            buffer[i] = Message {
                state: CachePadded::new(State::new(false, 0)),
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
    pub fn send(&mut self, payload: &T) -> u64 {
        let next_position = self.position.wrapping_add(1) % BUFFER_LEN;

        let seq_no = self.seq_no;
        let next_seq_no = seq_no.wrapping_add(1);

        let next_slot = &mut self.buffer[next_position];
        next_slot.state.store(true, next_seq_no, Ordering::SeqCst);

        let slot = &mut self.buffer[self.position];
        (*slot.payload).clone_from(payload);
        slot.state.store(false, seq_no, Ordering::SeqCst);

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
pub struct Receiver<'a, T, const BUFFER_LEN: usize> {
    position: usize, // points to next slot to be read
    buffer: &'a [Message<T>],
    recv_slot: T,
    recv_seq_no: u64,
}

impl<'a, T: Clone + Default + Debug, const BUFFER_LEN: usize> Receiver<'a, T, BUFFER_LEN> {
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
        assert!(BUFFER_LEN.is_power_of_two() && BUFFER_LEN > 1);
        if let Some(position) = buffer.iter().position(|x| x.state.load(Ordering::SeqCst).0) {
            Self {
                position,
                buffer,
                recv_slot: T::default(),
                recv_seq_no: 0,
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
    pub fn recv(&mut self) -> (u64, &T) {
        loop {
            let seq_no = self.load_state();
            let slot = &self.buffer[self.position];
            self.recv_slot.clone_from(&slot.payload);
            let (_, seq_no2) = self.try_load_state();
            if seq_no2 == seq_no {
                self.advance();
                return (seq_no, &self.recv_slot);
            }
        }
    }

    /// Non-blocking receive.
    ///
    /// Attempts to read a payload from the current slot.
    ///
    /// Returns:
    ///
    /// - `(seq_no, Some(&T))` on success, after advancing to the next slot.
    /// - `(seq_no, None)` if the slot is dirty or if it was overwritten while
    ///   copying (detected by a changed `seq_no`). In this case, the receiver
    ///   does **not** advance to the next slot.
    #[inline(always)]
    pub fn try_recv(&mut self) -> (u64, Option<&T>) {
        let (dirty, seq_no) = self.try_load_state();
        if !dirty {
            let slot = &self.buffer[self.position];
            self.recv_slot.clone_from(&(*slot.payload));
            let (_, seq_no2) = self.try_load_state();
            if seq_no2 == seq_no {
                self.advance();
                (seq_no, Some(&self.recv_slot))
            } else {
                (seq_no2, None)
            }
        } else {
            (seq_no, None)
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
    ///     let (seq_no, payload) = unsafe { rx.peek_unsafe() };
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
    pub unsafe fn peek_unsafe(&mut self) -> (u64, &T) {
        let seq_no = self.load_state();
        self.recv_seq_no = seq_no;
        let slot = &self.buffer[self.position];
        (seq_no, &slot.payload)
    }

    /// Non-blocking version of [`Receiver::peek_unsafe`].
    ///
    /// Returns:
    ///
    /// - `(seq_no, Some(&T))` if the slot is currently clean (committed).
    /// - `(seq_no, None)` if the slot is dirty (still being written).
    ///
    /// # Safety
    ///
    /// Same caveats as [`Receiver::peek_unsafe`]: the returned reference
    /// may be invalidated if the slot is overwritten, and must be validated
    /// with [`Receiver::advance_checked`].
    #[inline(always)]
    pub unsafe fn try_peek_unsafe(&mut self) -> (u64, Option<&T>) {
        let (dirty, seq_no) = self.try_load_state();
        let slot = &self.buffer[self.position];
        if !dirty {
            self.recv_seq_no = seq_no;
            (seq_no, Some(&slot.payload))
        } else {
            (seq_no, None)
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
        let (_, seq_no) = self.try_load_state();
        let valid = self.recv_seq_no == seq_no;
        self.position = self.position.wrapping_add(valid as usize) % BUFFER_LEN;
        valid
    }

    /// Advances the receiver past the current slot unconditionally.
    ///
    /// This is intended to be used after [`Receiver::load_state`] or.
    /// [`Receiver::try_load_state`]).
    #[inline(always)]
    pub fn advance(&mut self) {
        self.position = self.position.wrapping_add(1) % BUFFER_LEN;
    }

    /// Spin-waits until the current slot becomes clean (committed).
    ///
    /// This loops while the dirty flag is set, repeatedly loading the state.
    /// Once the slot is clean, it returns the observed `seq_no`.
    #[inline(always)]
    pub fn load_state(&self) -> u64 {
        // Spin-wait on `dirty` flag
        let slot = &self.buffer[self.position];
        let seq_no = loop {
            let (dirty, seq_no) = slot.state.load(Ordering::SeqCst);
            if !dirty {
                break seq_no;
            }
            // core::hint::spin_loop(); // Consider adding it. It adds asm instruction `pause`.
        };
        seq_no
    }

    /// Returns the current slot’s `(dirty, seq_no)` without blocking.
    ///
    /// This method performs a single atomic load of the slot state at the
    /// receiver's current position. It does **not** spin-wait for the slot to
    /// become clean; callers must handle the case where `dirty == true`.
    ///
    /// This is the non-blocking counterpart of [`Receiver::load_state`].
    #[inline(always)]
    pub fn try_load_state(&self) -> (bool, u64) {
        let slot = &self.buffer[self.position];
        slot.state.load(Ordering::SeqCst)
    }
}
