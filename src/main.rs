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
const PAYLOAD_SIZE: usize = 400;
const KEY: libc::key_t = 0x1234;
const BUFFER_LEN: usize = 4096;
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
use std::fmt::Debug;
//use std::sync::atomic::AtomicU64;
use portable_atomic::{AtomicU64, Ordering};

#[derive(Default)]
struct State(AtomicU64);

const LAST_BIT: u64 = 63;
const LAST_MASK: u64 = 1 << LAST_BIT;

impl State {
    fn new(last: bool, seq_no: u64) -> Self {
        debug_assert!(seq_no < LAST_MASK);
        Self(AtomicU64::new(seq_no | ((last as u64) << LAST_BIT)))
    }

    fn load(&self, order: Ordering) -> (bool, u64) {
        let v = self.0.load(order);
        (v & LAST_MASK != 0, v & !LAST_MASK)
    }

    fn store(&self, last: bool, seq_no: u64, order: Ordering) {
        let v = seq_no | ((last as u64) << LAST_BIT);
        self.0.store(v, order);
    }
}

impl Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (last, seq_no) = self.load(Ordering::SeqCst);
        f.debug_struct("State")
            .field("last", &last)
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

#[derive(Debug)]
pub struct Sender<'a, T> {
    position: usize, // points to `last`
    seq_no: u64,     // current `seq_no`
    buffer: &'a mut [Message<T>],
}

impl<'a, T: Clone + Default> Sender<'a, T> {
    pub fn new(buffer: &'a mut [Message<T>]) -> Self {
        assert!(!BUFFER_LEN >= 2);
        if let Some(_) = buffer.iter().position(|x| x.state.load(Ordering::SeqCst).0) {
            let mut tx = Self {
                position: 0,
                seq_no: 0,
                buffer,
            };
            tx.skip_to_last();
            tx
        } else {
            Self::with_init(buffer)
        }
    }

    pub fn with_init(buffer: &'a mut [Message<T>]) -> Self {
        assert!(!BUFFER_LEN >= 2);
        buffer[0] = Message {
            state: CachePadded::new(State::new(true, 0)),
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
            seq_no: 0,
            buffer,
        }
    }

    pub fn skip_to_last(&mut self) {
        loop {
            let slot = &self.buffer[self.position];
            let (last, seq_no) = slot.state.load(Ordering::SeqCst);
            if last {
                self.seq_no = seq_no;
                return;
            }
            self.position = (self.position + 1) % BUFFER_LEN;
        }
    }

    #[inline(always)]
    /// Sends a message into the channel by publishing the next slot and
    /// returning its sequence number.
    ///
    /// This channel provides **no backpressure**: when the buffer becomes
    /// full, sending a new message will **overwrite the oldest
    /// message**. It is the receiver’s responsibility to keep up with the
    /// sender if message loss is unacceptable. The receiver
    /// can detect message loss by observing gaps in the monotonically
    /// increasing `seq_no`.
    ///
    /// The channel’s protocol uses a `last` flag to indicate when a slot is
    /// fully written and safe to read. The receiver spins until it sees that
    /// the *previous* slot has `last = false`, meaning the sender has finished
    /// publishing the next slot. Conversely, the sender must not clear the
    /// previous slot’s `last` flag until the new payload is completely stored.
    ///
    /// This method follows that protocol by:
    /// - marking the next slot as `last = true` (publish-in-progress),
    /// - writing the payload,
    /// - then clearing `last = false` on the previous slot (publish complete).
    ///
    /// Readers relying on this protocol must ensure they only read slots that
    /// have been fully released (last==false); otherwise, they may observe partially written
    /// payloads.
    pub fn send(&mut self, payload: &T) -> u64 {
        let next_position = (self.position + 1) % BUFFER_LEN;
        let seq_no = self.seq_no;
        let next_seq_no = seq_no.wrapping_add(1);

        let next_slot = &mut self.buffer[next_position];
        next_slot.state.store(true, next_seq_no, Ordering::SeqCst);
        (*next_slot.payload).clone_from(payload);

        let slot = &mut self.buffer[self.position];
        slot.state.store(false, self.seq_no, Ordering::SeqCst);

        self.position = next_position;
        self.seq_no = next_seq_no;

        next_seq_no
    }
}

#[derive(Debug)]
pub struct Receiver<'a, T: Debug> {
    position: usize, // points to `last` (when all messages have been read)
    seq_no: u64,     // current `seq_no`
    buffer: &'a [Message<T>],
}

impl<'a, T: Debug> Receiver<'a, T> {
    pub fn new(buffer: &'a [Message<T>]) -> Self {
        assert!(!BUFFER_LEN >= 2);
        let mut rx = Self {
            position: 0,
            seq_no: 0,
            buffer,
        };
        rx.skip_to_last();
        rx
    }

    pub fn skip_to_last(&mut self) {
        loop {
            let slot = &self.buffer[self.position];
            let (last, seq_no) = slot.state.load(Ordering::SeqCst);
            if last {
                self.seq_no = seq_no;
                return;
            }
            self.position = (self.position + 1) % BUFFER_LEN;
        }
    }

    #[inline(always)]
    /// Returns the next available item in the buffer without any additional
    /// synchronization and advances the internal cursor.
    ///
    /// This is a low–level, unsafe variant intended for highly optimized code
    /// that knows exactly how the buffer is being used.
    ///
    /// # Safety
    /// Returns a shared reference (`&T`) to data that may be concurrently
    /// modified by other threads. The caller must ensure that no unsynchronized
    /// writes occur to the returned payload for the entire lifetime of the reference.
    ///
    /// Failing to uphold this invariant may cause data races, torn reads,
    /// or other undefined behavior.
    pub unsafe fn recv_unsafe(&mut self) -> (u64, &T) {
        // Wait on `last`
        let slot = &self.buffer[self.position];
        let _seq_no = loop {
            let (last, seq_no) = slot.state.load(Ordering::SeqCst);
            if !last {
                break seq_no;
            }
            // core::hint::spin_loop(); // Consider adding it. It adds `pause`.
        };

        loop {
            let next_position = (self.position + 1) % BUFFER_LEN;
            let next_slot = &self.buffer[next_position];
            let (last, seq_no) = next_slot.state.load(Ordering::SeqCst);
            if last || seq_no != 0 {
                // or seq_no > self.seq_no
                self.position = next_position;
                self.seq_no = seq_no.wrapping_add(1);
                return (seq_no, &next_slot.payload); // ???
            }
        }
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
