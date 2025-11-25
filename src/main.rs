use std::env;
use std::io;
use std::ptr;
use std::slice;
//use std::io::Write;

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

const PAYLOAD_SIZE: usize = 400;
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
        tx.send(&payload);
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
        let (seq_no, ts1) = unsafe {
            let (seq_no, payload) = rx.recv_unsafe();
            (seq_no, payload.timestamp) // we may have UB here
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

use crossbeam::utils::CachePadded;
use portable_atomic::{AtomicU64, Ordering};
use std::fmt::Debug;

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
    fn new(dirty: bool, seq_no: u64) -> Self {
        debug_assert!(seq_no < DIRTY_MASK);
        Self(AtomicU64::new(seq_no | ((dirty as u64) << DIRTY_BIT)))
    }

    fn load(&self, order: Ordering) -> (bool, u64) {
        let v = self.0.load(order);
        (v & DIRTY_MASK != 0, v & !DIRTY_MASK)
    }

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
    pub fn new(buffer: &'a mut [Message<T>]) -> Self {
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

    pub fn with_init(buffer: &'a mut [Message<T>]) -> Self {
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

#[derive(Debug)]
pub struct Receiver<'a, T: Debug> {
    position: usize, // points to `dirty` slot, i.e. the slot being written
    seq_no: u64,     // current `seq_no`, i.e. the `seq_no` expected to be written
    buffer: &'a [Message<T>],
}

impl<'a, T: Debug> Receiver<'a, T> {
    pub fn new(buffer: &'a [Message<T>]) -> Self {
        if let Some(position) = buffer.iter().position(|x| x.state.load(Ordering::SeqCst).0) {
            let (_, seq_no) = buffer[position].state.load(Ordering::SeqCst);
            Self {
                position,
                seq_no,
                buffer,
            }
        } else {
            panic!("Uninitialized buffer!");
        }
    }

    #[inline(always)]
    pub unsafe fn recv_unsafe(&mut self) -> (u64, &T) {
        // Spin-wait on `dirty` flag
        let slot = &self.buffer[self.position];
        let seq_no = loop {
            let (dirty, seq_no) = slot.state.load(Ordering::SeqCst);
            if !dirty {
                break seq_no;
            }
            // core::hint::spin_loop(); // Consider adding it. It adds asm instruction `pause`.
        };

        self.position = self.position.wrapping_add(1) % BUFFER_LEN;
        self.seq_no = seq_no.wrapping_add(1);

        (seq_no, &slot.payload)
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
