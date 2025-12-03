use crate::{map_shared_memory, unmap_shared_memory};
use crossbeam_utils::CachePadded;
use std::cell::Cell;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Creates a shared-memory channel backed by a `ShmBuffer`.
///
/// This is the primary constructor for an inter-process channel.  
/// Both the `Sender` and `Receiver` operate on the same underlying
/// shared-memory buffer.
///
/// The channel is defined for payload types `T` where `T: Copy + Clone`.  
/// The `Copy` bound is required because payloads are stored
/// directly in inter-process shared memory and must be trivially copyable.  
///
/// # Capacity
///
/// The `capacity` parameter is rounded up to the next power of two,
/// with a minimum of 2, and this value becomes the ring buffer's
/// effective capacity. This rounding simplifies wrap-around operations
/// inside the circular buffer.
///
/// # Shared Memory Name
///
/// The `shm_name` must follow POSIX shared-memory naming rules:
///
/// * it **must start with `'/'`**, e.g. `"/my-channel"`  
/// * it must contain no other `'/'` characters  
///
/// Attempting to create a region with a name that already exists will
/// reuse the same region.
///
/// # Parameters
///
/// * `shm_name` — Name of the POSIX shared-memory region  
/// * `capacity` — Minimum buffer capacity (rounded up to a power of two)
///
/// # Returns
///
/// A `(Sender, Receiver)` pair that both operate on the same shared-memory
/// ring buffer.  
///
/// Both types are `Send` but not `Sync`, ensuring they may be moved between
/// threads but cannot be shared concurrently across them.  
///
/// The `Receiver` is clonable, allowing multiple independent consumers that
/// each maintain their own read position.  
///
/// The `Sender` is **not** clonable, preserving the single-producer
/// invariant for the shared ring buffer.
///
/// # Errors
///
/// This function returns an error if:
///
/// * the shared-memory object cannot be created
/// * the name is invalid or does not begin with `'/'`
/// * or any OS-level shared-memory operation fails
pub fn channel<T: Clone + Copy + Default + Send + 'static>(
    shm_name: impl AsRef<str>,
    capacity: usize,
) -> std::io::Result<(Sender<T, ShmBuffer<T>>, Receiver<T, ShmBuffer<T>>)> {
    let buffer = ShmBuffer::try_new(capacity, shm_name.as_ref())?;
    Ok((Sender::new(buffer.clone()), Receiver::new(buffer)))
}

/// Creates an in-process channel backed by a `HeapBuffer`.
///
/// This is the non–shared-memory variant of the channel constructor.
/// Both the `Sender` and `Receiver` operate on the same heap-allocated
/// ring buffer and are intended for single-process use.
///
/// # Capacity
///
/// The `capacity` parameter is rounded up to the next power of two.
/// The resulting ring buffer capacity is always a power of two and
/// is never smaller than 2, regardless of the input value. This rounding
/// simplifies wrap-around operations inside the circular buffer.
///
/// Unlike the shared-memory version, `T` does **not** need to implement
/// `Copy`, since the buffer resides entirely within a single process.
///
/// # Parameters
///
/// * `capacity` — Minimum requested capacity (rounded up to a power of two)
///
/// # Returns
///
/// A `(Sender, Receiver)` pair that both reference the same
/// heap-allocated buffer.
///
/// Both types are `Send` but not `Sync`, ensuring they may be moved between
/// threads but cannot be shared concurrently across them.  
///
/// The `Receiver` is clonable, allowing multiple independent consumers that
/// each maintain their own read position.  
///
/// The `Sender` is **not** clonable, preserving the single-producer
/// invariant for the shared ring buffer.

pub fn local_channel<T: Clone + Default + Send>(
    capacity: usize,
) -> (Sender<T, HeapBuffer<T>>, Receiver<T, HeapBuffer<T>>) {
    let buffer = HeapBuffer::new(capacity);
    (Sender::new(buffer.clone()), Receiver::new(buffer))
}

/// A storage abstraction for the ring buffer used by the channels.
///
/// Implementors provide access to per-slot `Message<T>` entries and
/// define the effective capacity of the underlying buffer. Both
/// shared-memory and heap-based buffers implement this trait.
///
/// The buffer is conceptually a circular ring indexed by sequence
/// numbers (`seq_no`). Sequence numbers grow monotonically without
/// bound. The mapping from `seq_no` to an actual slot index is defined
/// by the implementor. This typically involves wrapping or
/// masking using the buffer's capacity.
pub trait Buffer<T> {
    /// Returns an immutable reference to the slot corresponding to
    /// the given sequence number.
    unsafe fn slot(&self, seq_no: u64) -> &Message<T>;

    /// Returns a mutable reference to the slot corresponding to the
    /// given sequence number.
    ///
    /// Implementors must ensure that the returned reference is valid
    /// and uniquely borrowed.
    unsafe fn slot_mut(&self, seq_no: u64) -> &mut Message<T>;

    /// Returns the effective capacity of the ring buffer.
    ///
    /// The capacity **must always be a power of two**, and never less
    /// than `2`. This requirement enables efficient index wrapping
    /// using bit-masking (e.g., `index = seq_no & (capacity - 1)`).
    fn capacity(&self) -> usize;
}

/// A heap-allocated ring buffer used as backing storage for a channel.
///
/// `HeapBuffer` owns a contiguous array of [`Message<T>`] values
/// allocated on the heap. It implements the [`Buffer<T>`] trait and is
/// used by [`local_channel`] to provide an in-process channel.
///
/// The buffer has a power-of-two capacity, allowing sequence numbers
/// to be mapped to slot indices by simple wrap-around.
///
/// Internally, the memory is owned by a boxed slice but also exposed
/// as a raw pointer for efficient lock-free access. The raw pointer
/// always refers to the same allocation as the boxed slice, and the
/// slice ensures proper lifetime management and deallocation.
#[derive(Debug)]
pub struct HeapBuffer<T> {
    /// Owns the memory containing the ring buffer slots.
    /// Freed automatically when the buffer is dropped.
    _boxed: Box<[Message<T>]>,

    /// Raw pointer to the first element of the same allocation.
    /// Used internally for lock-free indexing.
    ptr: NonNull<Message<T>>,

    /// Effective ring buffer capacity (always a power of two ≥ 2).
    capacity: usize,

    /// Mask used to map `seq_no` values into valid slot indices.
    /// Equal to `capacity - 1`.
    capacity_mask: usize,
}

unsafe impl<T: Send> Send for HeapBuffer<T> {}
unsafe impl<T: Send> Sync for HeapBuffer<T> {}

impl<T> Buffer<T> for HeapBuffer<T> {
    #[inline(always)]
    fn capacity(&self) -> usize {
        self.capacity
    }

    #[inline(always)]
    unsafe fn slot_mut(&self, seq_no: u64) -> &mut Message<T> {
        unsafe { self.ptr.add(seq_no as usize & self.capacity_mask).as_mut() }
    }

    #[inline(always)]
    unsafe fn slot(&self, seq_no: u64) -> &Message<T> {
        unsafe { self.ptr.add(seq_no as usize & self.capacity_mask).as_ref() }
    }
}

impl<T: Default> HeapBuffer<T> {
    /// Creates a new heap-allocated ring buffer.
    ///
    /// The requested `capacity` is rounded up to the next power of two,
    /// with a minimum value of `2`. The resulting value becomes the
    /// effective capacity of the ring buffer.
    ///
    /// All slots are initialized using [`Message::default`], ensuring
    /// that every position in the ring contains a valid `Message<T>`
    /// at construction.
    ///
    /// The underlying storage is allocated as a boxed slice and also
    /// exposed through a raw pointer for efficient lock-free indexing.
    /// The boxed slice guarantees proper deallocation when the buffer
    /// is dropped.
    ///
    /// # Returns
    ///
    /// An [`Arc`] to the newly created [`HeapBuffer`], allowing it to be
    /// safely shared between the sender and receiver.
    ///
    /// # Implementation Notes
    ///
    /// * `capacity_mask` is always `capacity - 1`, enabling fast
    ///   wrap-around using bit-masking.
    /// * `ptr` points to the same allocation owned by `_boxed`; it is
    ///   safe to use as long as the `Arc<HeapBuffer<T>>` remains alive.
    fn new(capacity: usize) -> Arc<Self> {
        let capacity = capacity.max(2).next_power_of_two();
        let capacity_mask = capacity - 1;
        let mut v: Vec<Message<T>> = Vec::with_capacity(capacity);
        v.resize_with(capacity, Message::default);
        let boxed = v.into_boxed_slice();
        let ptr = unsafe { NonNull::new_unchecked(boxed.as_ptr() as *mut Message<T>) };
        Arc::new(Self {
            _boxed: boxed,
            ptr,
            capacity,
            capacity_mask,
        })
    }
}

/// A ring buffer backed by POSIX shared memory.
///
/// `ShmBuffer` provides typed access to a region of shared memory
/// containing a contiguous array of [`Message<T>`] values. It implements
/// the [`Buffer<T>`] trait and is used by [`channel`] to create
/// inter-process channels.
///
/// Like `HeapBuffer`, this structure represents a power-of-two–sized
/// ring. Sequence numbers are mapped into slot indices by wrapping the
/// sequence number into range using a bitmask (`capacity_mask`).
///
/// The underlying shared-memory object is created or opened externally,
/// and remains valid for the lifetime of the `ShmBuffer`. When the last
/// `Arc<ShmBuffer<T>>` is dropped, the region is automatically unmapped.
#[derive(Debug)]
pub struct ShmBuffer<T> {
    ptr: NonNull<Message<T>>,
    capacity: usize,
    capacity_mask: usize,
}

unsafe impl<T: Send> Send for ShmBuffer<T> {}
unsafe impl<T: Send> Sync for ShmBuffer<T> {}

impl<T> Buffer<T> for ShmBuffer<T> {
    #[inline(always)]
    fn capacity(&self) -> usize {
        self.capacity
    }

    #[inline(always)]
    unsafe fn slot_mut(&self, seq_no: u64) -> &mut Message<T> {
        unsafe { self.ptr.add(seq_no as usize & self.capacity_mask).as_mut() }
    }

    #[inline(always)]
    unsafe fn slot(&self, seq_no: u64) -> &Message<T> {
        unsafe { self.ptr.add(seq_no as usize & self.capacity_mask).as_ref() }
    }
}

impl<T: Default> ShmBuffer<T> {
    /// Creates a shared-memory–backed ring buffer.
    ///
    /// The requested `capacity` is rounded up to the next power of two,
    /// with a minimum value of `2`. The resulting value becomes the
    /// effective capacity of the ring buffer.
    ///
    /// This constructor maps a POSIX shared-memory region identified by
    /// `shm_name` and interprets it as a contiguous array of
    /// `Message<T>`. The region must be large enough to hold `capacity`
    /// such messages.
    ///
    /// The shared-memory name must follow POSIX conventions (beginning
    /// with `'/'`, containing no other slashes). On Linux, the underlying
    /// object is created under `/dev/shm`.
    ///
    /// # Safety and Initialization
    ///
    /// The shared memory is assumed to already contain valid
    /// `Message<T>` values or to be properly initialized elsewhere.
    /// `ShmBuffer` does **not** zero or construct messages; it merely
    /// maps the region and provides typed access to it.
    ///
    /// # Returns
    ///
    /// An [`Arc`] containing the newly created `ShmBuffer` on success.
    ///
    /// # Errors
    ///
    /// Returns an error if the shared-memory region cannot be created
    /// or mapped.
    fn try_new(capacity: usize, shm_name: &str) -> std::io::Result<Arc<Self>> {
        let capacity = capacity.max(2).next_power_of_two();
        let capacity_mask = capacity - 1;
        let ptr = map_shared_memory(shm_name, capacity * std::mem::size_of::<Message<T>>())?;
        Ok(Arc::new(Self {
            ptr: ptr.cast(),
            capacity,
            capacity_mask,
        }))
    }
}

impl<T> Drop for ShmBuffer<T> {
    /// Unmaps the underlying shared-memory region.
    ///
    /// This method is automatically invoked when the last `Arc<ShmBuffer<T>>`
    /// is dropped. It calls `unmap_shared_memory` with the pointer and
    /// capacity originally used to map the region.
    ///
    /// # Panics
    ///
    /// Panics if unmapping the shared-memory region fails. This is considered
    /// an unrecoverable error, as leaking or corrupting a shared-memory mapping
    /// can break other processes relying on the same region.
    fn drop(&mut self) {
        unsafe {
            unmap_shared_memory(
                self.ptr.cast(),
                self.capacity * std::mem::size_of::<Message<T>>(),
            )
            .expect("ShmBuffer::drop failed");
        }
    }
}

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
    #[inline(always)]
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
        debug_assert!(seq_no < DIRTY_MASK);
        let v = seq_no | ((dirty as u64) << DIRTY_BIT);
        self.0.store(v, order);
    }
}

impl Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (dirty, seq_no) = self.load(Ordering::Acquire);
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

/// The channel’s sending handle, restricted to a single producer.
///
/// This channel provides **no backpressure**: when the buffer becomes full,
/// sending a new message will **overwrite the oldest message**. It is the
/// receiver’s responsibility to keep up with the sender if message loss is
/// unacceptable. The receiver can detect message loss by observing gaps in
/// the monotonically increasing `seq_no`.
///
/// Receivers must only read the payload from slots that are **not dirty**,
/// and after consuming the payload they must verify that the `seq_no` in the
/// slot is unchanged. If `seq_no` changed, the slot was overwritten and the
/// payload must be discarded.
///
/// The `Sender` is intentionally **not `Clone`**, and **not `Sync`**.
/// This enforces strict single-producer semantics: only one `Sender` instance can
/// exist for a given buffer, and it cannot be shared across threads.
///
/// # Protocol
///
/// The sender keeps at least one dirty slot at all times, allowing receivers to
/// detect progress and catch up by scanning forward until they find a dirty
/// slot and then waiting for dirty transitions.
///
/// # Fast wrap-around
///
/// If the sender wraps and overwrites a slot while a receiver is reading it,
/// the receiver can use the `seq_no` to detect this: after reading the payload,
/// it checks whether the `seq_no` in the slot is unchanged. If it changed, the
/// slot was overwritten and the receiver must discard the payload.
///
/// # Safety
///
/// When using a shared-memory backend, it is **undefined behavior** to construct
/// more than one channel over the same shared-memory region (for example, by
/// reopening the same named segment twice). Each shared-memory buffer must be
/// owned by exactly one logical channel (one `Sender` and its corresponding
/// receivers). Violating this requirement can break the single-writer invariants
/// assumed by the implementation and result in undefined behavior.
#[derive(Debug)]
pub struct Sender<T, B: Buffer<T>> {
    /// Current monotonically increasing `seq_no`.
    ///
    /// The sequence number associated with the **dirty slot**—the slot that
    /// the sender will write to next.
    ///
    /// That slot is located at:
    ///
    /// - `seq_no % capacity`  
    /// - or, more efficiently (since capacity is a power of two):
    ///   `seq_no & capacity_mask`
    seq_no: Cell<u64>,

    /// Shared backing buffer for the ring.
    buffer: Arc<B>,

    _marker: PhantomData<T>,
}

impl<T: Clone + Default + Send, B: Buffer<T>> Sender<T, B> {
    /// Constructs a sender from an existing ring buffer.
    ///
    /// If the buffer already contains a dirty slot with a non-zero `seq_no`,
    /// this picks up from the position and sequence number that the previous
    /// sender left off at.
    ///
    /// If no such dirty slot is found, this falls back to [`Sender::with_init`]
    /// and initializes the entire ring buffer to a valid starting state.
    fn new(buffer: Arc<B>) -> Self {
        if let Some(i) = (0..buffer.capacity() as u64).position(|i| {
            let (dirty, seq_no) = unsafe { buffer.slot(i).state.load(Ordering::Acquire) };
            dirty && seq_no != 0
        }) {
            let (_, seq_no) = unsafe { buffer.slot(i as u64).state.load(Ordering::Acquire) };
            Self {
                seq_no: Cell::new(seq_no),
                buffer,
                _marker: PhantomData,
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
    /// All other slots are initialized as *empty*:
    ///
    /// - `dirty = false`
    /// - `seq_no = 0`
    ///
    /// This establishes a valid starting state with a single dirty slot and
    /// `self.seq_no = 1`, ready for the first call to [`reserve`](Self::reserve).
    fn with_init(buffer: Arc<B>) -> Self {
        for i in 0..buffer.capacity() {
            unsafe {
                *buffer.slot_mut(i as u64) = Message {
                    state: CachePadded::new(State::new(false, 0)),
                    payload: CachePadded::new(T::default()),
                }
            };
        }
        unsafe {
            *buffer.slot_mut(1) = Message {
                state: CachePadded::new(State::new(true, 1)),
                payload: CachePadded::new(T::default()),
            }
        };

        Self {
            seq_no: Cell::new(1),
            buffer,
            _marker: PhantomData,
        }
    }

    /// Reserves a slot in the ring buffer for writing the payload.
    ///
    /// This is an HFT-style API:
    ///
    /// - No backpressure and no blocking.
    /// - Always returns a writable slot; it is the caller's responsibility
    /// to keep up and tolerate overwrites if necessary.
    ///
    /// Returns the current `seq_no` and a mutable reference to the payload of
    /// the next writable slot.
    ///
    /// After `reserve`, the caller typically writes the payload and then
    /// calls `commit`.
    ///
    /// # Returns
    ///
    /// A tuple `(seq_no, payload_ref)` where:
    ///
    /// - `seq_no` is the sequence number associated with this slot.
    /// - `payload_ref` is a mutable reference to the `T` stored in the slot.
    ///
    /// # Notes
    ///
    /// This call is effectively a no-op, as the sender always maintains a
    /// reserved slot ready for writing. It simply exposes that slot without
    /// performing any state transitions, so calling `reserve` repeatedly—or
    /// even without a matching `commit`—is harmless.
    #[inline(always)]
    pub fn reserve(&self) -> (u64, &mut T) {
        let seq_no = self.seq_no.get();
        unsafe { (seq_no, &mut self.buffer.slot_mut(seq_no).payload) }
    }

    /// Commits (publishes) the reserved slot.
    ///
    /// This finalizes and publishes the payload that was writtent into the
    /// reserved slot.
    ///
    /// Calling `commit` without `reserve` is safe and semantically equivalent to:
    ///
    /// 1. Calling `reserve()`.  
    /// 2. Performing **no writes** to the returned `&mut T`.  
    /// 3. Calling `commit()`.
    #[inline(always)]
    pub fn commit(&self) {
        let seq_no = self.seq_no.get();
        let next_seq_no = seq_no.wrapping_add(1);
        self.seq_no.set(next_seq_no);
        unsafe {
            self.buffer
                .slot_mut(next_seq_no)
                .state
                .store(true, next_seq_no, Ordering::Release);
        }
        unsafe {
            self.buffer
                .slot_mut(seq_no)
                .state
                .store(false, seq_no, Ordering::Release);
        }
    }

    /// Sends a message by cloning into the reserved slot.
    ///
    /// This is a convenience wrapper around [`Self::reserve`] and
    /// [`Self::commit`] that clones the provided `payload` into the
    /// buffered slot.
    ///
    /// # Returns
    ///
    /// The sequence number associated with the sent message.
    ///
    /// # Performance
    ///
    /// This variant performs a `Clone` of `payload`. Prefer [`Self::send`]
    /// when you can move the value instead.
    #[inline(always)]
    pub fn send_from(&self, payload: &T) -> u64 {
        let (seq_no, p) = self.reserve();
        p.clone_from(payload);
        self.commit();
        seq_no
    }

    /// Sends a message by moving it into the reserved slot.
    ///
    /// This is a convenience wrapper around [`Self::reserve`] and
    /// [`Self::commit`] that moves `payload` into the buffered slot.
    ///
    /// # Returns
    ///
    /// The sequence number associated with the sent message.
    ///
    /// # Performance
    ///
    /// This variant avoids an extra clone and is generally preferred when
    /// `payload` can be moved.
    #[inline(always)]
    pub fn send(&self, payload: T) -> u64 {
        let (seq_no, p) = self.reserve();
        *p = payload;
        self.commit();
        seq_no
    }
}

/// The channel’s receiving handle, allowing multiple independent consumers.
///
/// A `Receiver`:
///
/// - never mutates the structure of the ring buffer,
/// - maintains its own independent read position (`seq_no`),
/// - can run concurrently with other receivers, and
/// - holds a private payload element used to return stable references .
///
/// Each receiver may lag behind the sender. If the sender wraps around and
/// overwrites the receiver’s target slot, the receiver detects this via a
/// mismatch in the slot’s `seq_no`.
///
/// # Reading Protocol
///
/// Reading from the ring buffer follows this invariant:
///
/// - A slot is readable when its **dirty flag is `false`**  
///   (meaning the sender has completed writing it).
///
/// After copying the payload out of a readable slot, the receiver must verify
/// that the slot’s `seq_no` is unchanged.  
///
/// - If `seq_no` unchanged, then the payload is valid.  
/// - If `seq_no` changed, then the slot was overwritten and the payload must be
///   discarded.
///
/// Because each receiver tracks its own `seq_no`, different receivers may
/// progress at different speeds without interfering with each other.
///
/// # Overwrite Detection
///
/// If a receiver falls behind and attempts to read a slot that was already
/// overwritten, the `seq_no` comparison reliably detects the loss.
///  
/// The receiver can then:
///
/// - discard the payload,  
/// - resynchronize to the new sequence number,  
/// - and continue receiving.
#[derive(Debug)]
pub struct Receiver<T, B: Buffer<T>> {
    /// The receiver's current `seq_no`, i.e. the `seq_no` of expected message.
    seq_no: Cell<u64>,

    /// Shared backing buffer for the ring.
    buffer: Arc<B>,

    /// A reusable scratch buffer that holds one instance of `T`.
    ///
    /// Safe APIs that must return a stable reference to a payload copy the
    /// received data into this buffer. This ensures the returned reference
    /// remains valid even if the sender overwrites the underlying ring slot
    /// immediately after the read.
    recv_payload: T,

    _marker: PhantomData<T>,
}

impl<T: Clone + Default, B: Buffer<T>> Receiver<T, B> {
    /// Constructs a receiver positioned at the current dirty slot.
    ///
    /// This scans the buffer for the slot currently marked dirty
    /// (the one the sender is writing) and initializes the receiver’s
    /// `seq_no` to that value.
    ///
    /// # Panics
    ///
    /// Panics if no dirty slot is found, which indicates an uninitialized or
    /// corrupted buffer state.
    fn new(buffer: Arc<B>) -> Self {
        if let Some(i) = (0..buffer.capacity() as u64).position(|i| {
            let (dirty, seq_no) = unsafe { buffer.slot(i).state.load(Ordering::Acquire) };
            dirty && seq_no != 0
        }) {
            let (_, seq_no) = unsafe { buffer.slot(i as u64).state.load(Ordering::Acquire) };
            Self {
                seq_no: Cell::new(seq_no),
                buffer: buffer,
                recv_payload: T::default(),
                _marker: PhantomData,
            }
        } else {
            panic!("Uninitialized buffer!"); // TODO: will panic if we start receiver before sender
        }
    }

    /// Blocking receive.
    ///
    /// Waits for the current slot to become clean (committed), copies its
    /// payload into the receiver’s internal buffer, and validates that the slot
    /// was not overwritten during the copy.
    ///
    /// On success:
    ///
    /// - returns the `seq_no` and a stable reference to the payload,
    /// - and advances the receiver to the next sequence number.
    ///
    /// If the slot was overwritten, the method retries until a stable read
    /// is achieved.
    ///
    /// # Message Loss
    ///
    /// If the receiver cannot keep up with the sender, **gaps in the observed
    /// `seq_no` are possible**. Such gaps indicate that one or more messages were
    /// overwritten by the sender before the receiver could consume them. The
    /// receiver automatically advances to the next available message, ensuring
    /// forward progress even under heavy load.
    #[inline(always)]
    pub fn recv(&mut self) -> (u64, &T) {
        loop {
            let seq_no = self.wait();
            unsafe {
                self.recv_payload
                    .clone_from(&*self.buffer.slot(seq_no).payload)
            };
            let (_, seq_no2) = self.load_state(seq_no);
            if seq_no2 == seq_no {
                self.seq_no.set(seq_no.wrapping_add(1));
                return (seq_no, &self.recv_payload);
            }
            // We do not need to reset seq_no because it will point to the same slot anyways
        }
    }

    /// Non-blocking receive attempt.
    ///
    /// Attempts to read the current slot **without blocking**.
    ///
    /// Returns:
    ///
    /// - `(seq_no, Some(&T))` if the slot is clean and the value was **not**
    ///   overwritten during the read.
    ///
    /// - `(seq_no, None)` if:
    ///   - the slot is still dirty, or
    ///   - the slot was overwritten during the read.
    ///
    /// Behavior:
    ///
    /// - If the slot is dirty, the receiver **does not advance**.
    /// - If the slot was overwritten, no payload is delivered, but the receiver
    ///   updates its state to the newly observed `seq_no`.
    #[inline(always)]
    pub fn try_recv(&mut self) -> (u64, Option<&T>) {
        let (dirty, seq_no) = self.load_state(self.seq_no.get());
        if !dirty {
            unsafe {
                self.recv_payload
                    .clone_from(&*self.buffer.slot(seq_no).payload);
            }
            let (_, seq_no2) = self.load_state(seq_no);
            if seq_no2 == seq_no {
                self.seq_no.set(seq_no.wrapping_add(1));
                (seq_no, Some(&self.recv_payload))
            } else {
                self.seq_no.set(seq_no2);
                (seq_no2, None)
            }
        } else {
            self.seq_no.set(seq_no);
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
    /// Use this together with [`Receiver::advance`] to validate the peek:
    ///
    /// ```ignore
    /// loop {
    ///     let (seq_no, payload_ref) = unsafe { rx.peek_unsafe() };
    ///     // ... use `payload_ref` speculatively ...
    ///     if rx.advance() {
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
    /// [`Receiver::advance`] and check its return value before relying on
    /// any work derived from `payload`.
    #[inline(always)]
    pub unsafe fn peek_unsafe(&self) -> (u64, &T) {
        let seq_no = self.wait();
        self.seq_no.set(seq_no);
        unsafe { (seq_no, &*self.buffer.slot(seq_no).payload) }
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
    /// with [`Receiver::advance`].
    #[inline(always)]
    pub unsafe fn try_peek_unsafe(&self) -> (u64, Option<&T>) {
        let (dirty, seq_no) = self.load_state(self.seq_no.get());
        self.seq_no.set(seq_no);
        let value = unsafe { (!dirty).then_some(&*self.buffer.slot(seq_no).payload) };
        (seq_no, value)
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
    pub fn advance(&self) -> bool {
        let (_, seq_no) = self.load_state(self.seq_no.get());
        let valid = self.seq_no.get() == seq_no;
        self.seq_no.set(seq_no.wrapping_add(valid as u64));
        valid
    }

    /// Spin-waits until the current slot becomes clean (committed).
    ///
    /// This loops while the dirty flag is set, repeatedly loading the state
    /// from the slot that corrsponds to to `self.seq_no`.
    ///
    /// Once the slot is clean, it resets `self.seq_no` to observed `seq_no`
    /// and returns it.
    ///
    /// The observed `seq_no` can be `self.seq_no + self.buffer.capacity() * n`,
    /// where n >= 0.
    #[inline(always)]
    pub fn wait(&self) -> u64 {
        // Spin-wait on `dirty` flag
        let slot = unsafe { self.buffer.slot(self.seq_no.get()) };
        let seq_no = loop {
            let (dirty, seq_no) = slot.state.load(Ordering::Acquire);
            if !dirty {
                break seq_no;
            }
            core::hint::spin_loop(); // On x86_64 this adds asm instruction `pause`.
        };
        seq_no
    }

    /// Returns `(dirty, seq_no)` at `seq_no` position without blocking.
    #[inline(always)]
    fn load_state(&self, seq_no: u64) -> (bool, u64) {
        unsafe { self.buffer.slot(seq_no).state.load(Ordering::Acquire) }
    }
}

impl<T: Clone, B: Buffer<T>> Clone for Receiver<T, B> {
    fn clone(&self) -> Self {
        Self {
            seq_no: self.seq_no.clone(),
            buffer: Arc::clone(&self.buffer),
            recv_payload: self.recv_payload.clone(),
            _marker: PhantomData,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUF: usize = 4;

    #[test]
    fn state_packs_and_unpacks_clean() {
        let s = State::new(false, 123);
        let (dirty, seq) = s.load(Ordering::Acquire);
        assert!(!dirty);
        assert_eq!(seq, 123);

        // overwrite via store
        s.store(false, 999, Ordering::Release);
        let (dirty2, seq2) = s.load(Ordering::Acquire);
        assert!(!dirty2);
        assert_eq!(seq2, 999);
    }

    #[test]
    fn state_packs_and_unpacks_dirty() {
        let s = State::new(true, 42);
        let (dirty, seq) = s.load(Ordering::Acquire);
        assert!(dirty);
        assert_eq!(seq, 42);

        s.store(true, 777, Ordering::Release);
        let (dirty2, seq2) = s.load(Ordering::Acquire);
        assert!(dirty2);
        assert_eq!(seq2, 777);
    }

    #[test]
    fn sender_with_init_sets_single_dirty_slot() {
        // Allocate a heap-backed ring buffer
        let buffer = HeapBuffer::<u64>::new(BUF);

        // Initialize sender, which also initializes the ring buffer
        let _sender: Sender<u64, HeapBuffer<u64>> = Sender::with_init(buffer.clone());

        // After init:
        // - slot 1 must be dirty with seq_no = 1
        // - all other slots must be clean with seq_no = 0
        // - exactly one dirty slot must exist
        let mut dirty_count = 0;

        for idx in 0..buffer.capacity() {
            let slot = unsafe { buffer.slot(idx as u64) };
            let (dirty, seq_no) = slot.state.load(Ordering::Acquire);

            if idx == 1 {
                assert!(dirty, "slot 1 must be dirty after init");
                assert_eq!(seq_no, 1, "slot 0 seq_no must be 1 after init");
            } else {
                assert!(!dirty, "slot {} must be clean after init", idx);
                assert_eq!(seq_no, 0, "slot {} seq_no must be 0 after init", idx);
            }

            if dirty {
                dirty_count += 1;
            }
        }

        assert_eq!(dirty_count, 1, "exactly one slot must be dirty after init");
    }

    #[test]
    fn sender_send_writes_payload_and_advances_dirty_slot() {
        // assume BUF is your test capacity const; otherwise replace with a number
        let buffer = HeapBuffer::<u64>::new(BUF);
        let sender: Sender<u64, HeapBuffer<u64>> = Sender::with_init(buffer.clone());

        // Send first payload
        let seq1 = sender.send(11u64);
        assert_eq!(seq1, 1);

        // After first send:
        // - slot 1 should be clean with seq_no = 1 and payload = 11
        // - exactly one dirty slot exists (the next write target)
        let mut dirty_indices = Vec::new();
        for idx in 0..buffer.capacity() {
            let slot = unsafe { buffer.slot(idx as u64) };
            let (dirty, seq_no) = slot.state.load(Ordering::Acquire);

            if dirty {
                dirty_indices.push(idx);
            }

            if idx == 1 {
                assert!(!dirty, "slot 1 should be clean after first send");
                assert_eq!(seq_no, 1);
                assert_eq!(*slot.payload, 11);
            } else if idx == 2 {
                assert!(dirty, "slot 2 should be dirty after first send");
                assert_eq!(seq_no, 2);
                assert_eq!(*slot.payload, 0);
            }
        }

        assert_eq!(
            dirty_indices.len(),
            1,
            "there must still be exactly one dirty slot"
        );

        // Send a second payload
        let seq2 = sender.send(22u64);
        assert_eq!(seq2, 2);

        // Now slot 2 should be clean with seq_no = 2 and payload = 22
        let slot1 = unsafe { buffer.slot(2u64) };
        let (dirty1, seq1_again) = slot1.state.load(Ordering::Acquire);
        assert!(!dirty1, "slot 2 should be clean after second send");
        assert_eq!(seq1_again, 2);
        assert_eq!(*slot1.payload, 22);

        // Still only one dirty slot in the ring
        let dirty_count = (0..buffer.capacity())
            .filter(|&i| unsafe { buffer.slot(i as u64).state.load(Ordering::Acquire).0 })
            .count();

        assert_eq!(dirty_count, 1, "exactly one dirty slot must be preserved");
    }

    #[test]
    fn receiver_new_and_try_recv_before_send_returns_none() {
        // Initialize heap-backed buffer and sender (which initializes the ring)
        let buffer = HeapBuffer::<u64>::new(BUF);
        let _sender: Sender<u64, HeapBuffer<u64>> = Sender::with_init(buffer.clone());

        // Before we send anything, the only dirty slot is the initial one.
        let mut rx: Receiver<u64, HeapBuffer<u64>> = Receiver::new(buffer);

        // At this point, the receiver points at the initial dirty slot.
        // try_recv should see dirty == true and return None.
        let (seq, val) = rx.try_recv();
        assert!(val.is_none());
        assert_eq!(seq, 1, "seq_no should be the seq of the dirty slot");
    }

    #[test]
    fn send_then_recv_roundtrip() {
        let buffer = HeapBuffer::<u64>::new(BUF);
        let sender: Sender<u64, HeapBuffer<u64>> = Sender::with_init(buffer.clone());
        let mut rx: Receiver<u64, HeapBuffer<u64>> = Receiver::new(buffer);

        // First message
        let seq1 = sender.send(111u64);
        let (rseq1, v1) = rx.recv();
        assert_eq!(seq1, rseq1);
        assert_eq!(*v1, 111);

        // Second message
        let seq2 = sender.send(222u64);
        let (rseq2, v2) = rx.recv();
        assert_eq!(seq2, rseq2);
        assert_eq!(*v2, 222);
    }

    #[test]
    fn peek_unsafe_and_advance_zero_copy_pattern() {
        let buffer = HeapBuffer::<u64>::new(BUF);
        let sender: Sender<u64, HeapBuffer<u64>> = Sender::with_init(buffer.clone());
        let rx: Receiver<u64, HeapBuffer<u64>> = Receiver::new(buffer);

        // Produce a value
        let seq1 = sender.send(999u64);

        // Use the (peek, advance) pattern
        let (seen_seq, ts) = unsafe {
            loop {
                let (s, payload) = rx.peek_unsafe();
                let val = *payload;
                if rx.advance() {
                    break (s, val);
                }
            }
        };

        assert_eq!(seen_seq, seq1);
        assert_eq!(ts, 999);
    }

    #[test]
    fn reserve_without_write_publishes_default_value() {
        let buffer = HeapBuffer::<u64>::new(BUF);
        let sender: Sender<u64, HeapBuffer<u64>> = Sender::with_init(buffer.clone());
        let mut rx: Receiver<u64, HeapBuffer<u64>> = Receiver::new(buffer);

        // First reserve/commit without touching the payload
        let (seq, _payload_ref) = sender.reserve();
        sender.commit();

        // Receiver should see the default value in that slot
        let (rseq, v) = rx.recv();
        assert_eq!(rseq, seq, "seq_no must match between sender and receiver");
        assert_eq!(*v, u64::default(), "payload must be the default value");
    }

    #[test]
    fn reserve_write_then_commit_publishes_written_value() {
        let buffer = HeapBuffer::<u64>::new(BUF);
        let sender: Sender<u64, HeapBuffer<u64>> = Sender::with_init(buffer.clone());
        let mut rx: Receiver<u64, HeapBuffer<u64>> = Receiver::new(buffer);

        // Reserve a slot, write into it, then commit
        let (seq, payload_ref) = sender.reserve();
        *payload_ref = 42;
        sender.commit();

        // Receiver should observe exactly that value with the same seq
        let (rseq, v) = rx.recv();
        assert_eq!(rseq, seq, "seq_no must match between sender and receiver");
        assert_eq!(*v, 42, "payload written via reserve/commit must be visible");
    }

    #[test]
    fn send_from_and_send_roundtrip() {
        let buffer = HeapBuffer::<u64>::new(BUF);
        let sender: Sender<u64, HeapBuffer<u64>> = Sender::with_init(buffer.clone());
        let mut rx: Receiver<u64, HeapBuffer<u64>> = Receiver::new(buffer);

        // send_from(&T) clones into the buffer
        let original = 123u64;
        let seq1 = sender.send_from(&original);
        assert_eq!(original, 123, "send_from must not move the value");

        let (rseq1, v1) = rx.recv();
        assert_eq!(seq1, rseq1, "seq_no must match for send_from");
        assert_eq!(*v1, 123, "payload must match cloned value");

        // send(T) moves into the buffer
        let seq2 = sender.send(456u64);
        let (rseq2, v2) = rx.recv();
        assert_eq!(seq2, rseq2, "seq_no must match for send");
        assert_eq!(*v2, 456, "payload must match moved value");
    }

    #[test]
    fn try_recv_returns_some_after_send() {
        let buffer = HeapBuffer::<u64>::new(BUF);
        let sender: Sender<u64, HeapBuffer<u64>> = Sender::with_init(buffer.clone());
        let mut rx: Receiver<u64, HeapBuffer<u64>> = Receiver::new(buffer);

        // Initially, receiver should see no committed message
        let (_seq0, v0) = rx.try_recv();
        assert!(v0.is_none(), "no message should be available before send");

        // After sending, try_recv should return Some(...)
        let seq1 = sender.send(10);
        let (rseq1, v1) = rx.try_recv();
        assert_eq!(rseq1, seq1);
        assert!(v1.is_some());
        assert_eq!(*v1.unwrap(), 10);
    }

    #[test]
    fn recv_and_try_recv_interleave_correctly() {
        let buffer = HeapBuffer::<u64>::new(BUF);
        let sender: Sender<u64, HeapBuffer<u64>> = Sender::with_init(buffer.clone());
        let mut rx: Receiver<u64, HeapBuffer<u64>> = Receiver::new(buffer);

        // Send two messages
        let seq1 = sender.send(111);
        let seq2 = sender.send(222);

        // First message via blocking recv
        let (rseq1, v1) = rx.recv();
        assert_eq!(rseq1, seq1);
        assert_eq!(*v1, 111);

        // Second message via try_recv (non-blocking)
        let (rseq2, v2) = rx.try_recv();
        assert_eq!(rseq2, seq2);
        assert!(v2.is_some());
        assert_eq!(*v2.unwrap(), 222);

        // Now we should be at the next seq with nothing committed yet
        let (_seq3, v3) = rx.try_recv();
        assert!(v3.is_none(), "no further messages should be available");
    }

    #[test]
    fn try_peek_unsafe_returns_none_while_dirty_and_some_when_clean() {
        let buffer = HeapBuffer::<u64>::new(BUF);
        let sender: Sender<u64, HeapBuffer<u64>> = Sender::with_init(buffer.clone());
        let rx: Receiver<u64, HeapBuffer<u64>> = Receiver::new(buffer);

        // Immediately after init, the current slot is dirty (being written).
        // try_peek_unsafe should return None.
        let (_seq0, v0) = unsafe { rx.try_peek_unsafe() };
        assert!(v0.is_none(), "peek on dirty slot must return None");

        // Publish a value
        let seq1 = sender.send(777);

        // Now the current readable slot should be clean and peekable
        let (pseq, v1) = unsafe { rx.try_peek_unsafe() };
        assert_eq!(pseq, seq1, "peeked seq_no must match last sent");
        assert!(v1.is_some());
        assert_eq!(*v1.unwrap(), 777);
    }

    #[test]
    fn peek_unsafe_and_advance_detect_overwrite() {
        // Use a small buffer to make overwrites easy to trigger
        const SMALL_BUF: usize = 4;
        let buffer = HeapBuffer::<u64>::new(SMALL_BUF);
        let sender: Sender<u64, HeapBuffer<u64>> = Sender::with_init(buffer.clone());
        let rx: Receiver<u64, HeapBuffer<u64>> = Receiver::new(buffer);

        // Produce one value and peek it
        let first_seq = sender.send(1);

        let (peek_seq, payload_ref) = unsafe { rx.peek_unsafe() };
        assert_eq!(peek_seq, first_seq);
        assert_eq!(*payload_ref, 1);

        // Now advance the sender enough to overwrite this slot.
        // With capacity SMALL_BUF, sending SMALL_BUF more messages will lap the receiver.
        for i in 2..=(SMALL_BUF as u64 + 1) {
            sender.send(i);
        }

        // Now advance() should notice that the seq_no changed and fail.
        let ok = rx.advance();
        assert!(
            !ok,
            "advance() must return false when the peeked slot has been overwritten"
        );
    }

    #[test]
    fn peek_unsafe_and_advance_succeeds_without_overwrite() {
        let buffer = HeapBuffer::<u64>::new(BUF);
        let sender: Sender<u64, HeapBuffer<u64>> = Sender::with_init(buffer.clone());
        let rx: Receiver<u64, HeapBuffer<u64>> = Receiver::new(buffer);

        // Produce exactly one value
        let seq = sender.send(999);

        // Use the (peek, advance) zero-copy pattern
        let (seen_seq, val) = unsafe {
            loop {
                let (s, payload) = rx.peek_unsafe();
                let v = *payload;
                if rx.advance() {
                    break (s, v);
                }
                // In this test, we expect advance() to succeed on the first try.
            }
        };

        assert_eq!(seen_seq, seq);
        assert_eq!(val, 999);
    }

    //use std::sync::atomic::Ordering;

    // adjust if needed
    const CAP: usize = 8;

    #[test]
    fn local_channel_roundtrip_with_large_array_payload() {
        // Channel over a large fixed-size array payload.
        let (sender, mut receiver) = local_channel::<[u8; 5]>(CAP);

        // --- First message via send(T) ---

        let mut payload1 = [0u8; 5];
        for (i, b) in payload1.iter_mut().enumerate() {
            *b = (i % 4) as u8;
        }

        let seq1 = sender.send(payload1);
        let (rseq1, recv1) = receiver.recv();

        assert_eq!(seq1, rseq1, "seq_no must match for array payload");
        // Check a few sentinel positions, not just equality
        assert_eq!(recv1[0], 0);
        assert_eq!(recv1[1], 1);
        assert_eq!(recv1[4], 0);

        // After consuming first message, no more data should be ready
        let (_seq_after1, maybe_val1) = receiver.try_recv();
        assert!(
            maybe_val1.is_none(),
            "no further messages should be available after single recv"
        );

        // --- Second message via reserve/commit ---

        let (seq2, slot) = sender.reserve();
        // Write directly into the reserved slot
        for (i, b) in slot.iter_mut().enumerate() {
            *b = ((i * 3) % 4) as u8;
        }
        sender.commit();

        let (rseq2, recv2) = receiver.recv();
        assert_eq!(
            seq2, rseq2,
            "seq_no must match for reserve/commit array payload"
        );
        assert_eq!(recv2[0], 0);
        assert_eq!(recv2[1], 3);
        assert_eq!(recv2[2], 2);
        assert_eq!(recv2[3], 1);

        // Still, after consuming, try_recv should see nothing pending
        let (_seq_after2, maybe_val2) = receiver.try_recv();
        assert!(
            maybe_val2.is_none(),
            "no further messages should remain after second recv"
        );
    }

    #[test]
    fn local_channel_roundtrip_with_vec_payload_clone_and_move() {
        let (sender, mut receiver) = local_channel::<Vec<u64>>(CAP);

        // --- send_from(&T): payload is cloned into the buffer ---

        let original = vec![1u64, 2, 3, 4, 5];
        let seq1 = sender.send_from(&original);

        // original must remain usable and unchanged
        assert_eq!(original, vec![1, 2, 3, 4, 5]);

        let (rseq1, v1) = receiver.recv();
        assert_eq!(seq1, rseq1, "seq_no must match for send_from");
        assert_eq!(&*v1, &original, "received vec must match cloned payload");

        // --- send(T): payload is moved into the buffer ---

        let payload2 = vec![10u64, 20, 30];
        let seq2 = sender.send(payload2);
        // payload2 is moved and cannot be used here

        let (rseq2, v2) = receiver.recv();
        assert_eq!(seq2, rseq2, "seq_no must match for send(T)");
        assert_eq!(&*v2, &[10, 20, 30]);

        // --- try_recv should now report that no more messages are available ---

        let (_seq_after, maybe_val) = receiver.try_recv();
        assert!(
            maybe_val.is_none(),
            "no further messages should be available after consuming all sends"
        );

        // --- Mix send_from and send to ensure sequencing is consistent ---

        let base_vec = vec![100u64, 200, 300];
        let seq3 = sender.send_from(&base_vec);
        let seq4 = sender.send(vec![400u64, 500, 600]);

        let (rseq3, v3) = receiver.recv();
        assert_eq!(rseq3, seq3);
        assert_eq!(&*v3, &base_vec);

        let (rseq4, v4) = receiver.recv();
        assert_eq!(rseq4, seq4);
        assert_eq!(&*v4, &[400, 500, 600]);

        assert!(
            rseq4 > rseq3,
            "sequence numbers must be monotonically increasing"
        );
    }
}
