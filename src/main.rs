use std::env;
use std::io;
use std::ptr;
use std::slice;
use std::time::Duration;
use std::thread::sleep;

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

const KEY: libc::key_t = 0x1234;
const LEN: usize = 32;
const SIZE: usize = LEN * std::mem::size_of::<Message<u64>>();

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} writer|reader", args[0]);
        std::process::exit(1);
    }

    match args[1].as_str() {
        "writer" => writer(),
        "reader" => reader(),
        _ => {
            eprintln!("Usage: {} writer|reader", args[0]);
            std::process::exit(1);
        }
    }
}

fn writer() -> io::Result<()> {
    let shmid = unsafe { libc::shmget(KEY, SIZE, libc::IPC_CREAT | 0o600) };
    if shmid == -1 {
        return Err(io::Error::last_os_error());
    }

    let addr = unsafe { libc::shmat(shmid, ptr::null(), 0) };
    if addr == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }

    println!("writer: attached at {:p}, shmid = {}", addr, shmid);

    let buf = unsafe { slice::from_raw_parts_mut(addr as *mut Message<u64>, LEN) };
    println!("buf.len() = {}", buf.len());

    let mut tx = Sender::new(buf);
    loop {
        let ts = rdtscp();
        dbg!((tx.send(&ts), ts));
        sleep(Duration::from_millis(1000));
    }

    Ok(())
}

fn reader() -> io::Result<()> {
    let shmid = unsafe { libc::shmget(KEY, SIZE, 0o600) };
    if shmid == -1 {
        return Err(io::Error::last_os_error());
    }

    let addr = unsafe { libc::shmat(shmid, ptr::null(), 0) };
    if addr == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }

    println!("reader: attached at {:p}, shmid = {}", addr, shmid);

    let buf = unsafe { slice::from_raw_parts(addr as *const Message<u64>, LEN) };
    println!("buf.len() = {}", buf.len());

    let mut rx = Receiver::new(buf);
    loop {
        let (seq_no, ts0) = rx.recv();
        let ts1 = rdtscp();
        dbg!((seq_no, *ts0, ts1, ts1 - *ts0));
    }

    Ok(())
}

use crossbeam::utils::CachePadded;
use std::fmt::Debug;
use std::sync::atomic::Ordering;
use std::sync::atomic::{AtomicBool, AtomicU64};
use core::hint::spin_loop;

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

#[derive(Debug, Default)]
#[repr(C)]
struct Message<T: Sized> {
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
        assert!(!buffer.len() >= 2);
        let mut tx = Self {
            position: 0,
            seq_no: 0,
            buffer,
        };
        tx.skip_to_last();
        tx
    }

    pub fn with_init(&mut self, buffer: &'a mut [Message<T>]) -> Self {
        assert!(!buffer.len() >= 2);
        buffer[0] = Message {
            state: CachePadded::new(State::new(true, 0)),
            payload: CachePadded::new(T::default()),
        };
        for i in 1..buffer.len() {
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
            self.position = (self.position + 1) % self.buffer.len();
        }
    }


    #[inline(always)]
    pub fn send(&mut self, payload: &T) -> u64 {
        let next_position = (self.position + 1) % self.buffer.len();
        let seq_no = self.seq_no;
        let next_seq_no = seq_no.wrapping_add(1);

        let next_slot = &mut self.buffer[next_position];
        next_slot.state.store(true, next_seq_no, Ordering::SeqCst);
        (*next_slot.payload).clone_from(payload);

        let slot = &mut self.buffer[self.position];
        slot.state
            .store(false, self.seq_no, Ordering::SeqCst);

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
        assert!(!buffer.len() >= 2);
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
            self.position = (self.position + 1) % self.buffer.len();
        }
    }


    #[inline(always)]
    pub fn recv(&mut self) -> (u64, &T) {
        // Wait on `last`
        let slot = &self.buffer[self.position];
        let _seq_no = loop {
            let (last, seq_no) = slot.state.load(Ordering::SeqCst);
            if !last {
                break seq_no;
            }
            spin_loop();
        };

        loop {
            let next_position = (self.position + 1) % self.buffer.len();
            let next_slot = &self.buffer[next_position];
            let (last, seq_no) = next_slot.state.load(Ordering::SeqCst);
            if last || seq_no != 0 { // or seq_no > self.seq_no
                self.position = next_position;
                self.seq_no = seq_no.wrapping_add(1);
                return (seq_no, &next_slot.payload);
            }
        }
    }
}
