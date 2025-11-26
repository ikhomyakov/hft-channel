use std::env;
use std::io;
use std::ptr;
use std::slice;
use ichannel::{BUFFER_LEN, BUFFER_SIZE, Payload, PAYLOAD_SIZE, Message, Trials, Sender, Receiver};

/// Returns timestamp in ns
#[cfg(unix)]
#[inline(always)]
fn mono_time_ns() -> u64 {
    use libc::{CLOCK_MONOTONIC, clock_gettime, timespec};
    unsafe {
        let mut ts = timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        clock_gettime(CLOCK_MONOTONIC, &mut ts);
        (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64)
    }
}

const KEY: libc::key_t = 0x1234;

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} writer|reader|both", args[0]);
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

    const TRIALS: usize = 100_000;
    let mut trials = Trials::with_capacity(TRIALS);
    let mut trials2 = Trials::with_capacity(TRIALS);

    let mut payload = Payload::<PAYLOAD_SIZE>::default();

    let mut tx = Sender::new(buf);

    loop {
        let ts0 = mono_time_ns();
        payload.timestamp = ts0;
        tx.send(&payload);
        let ts1 = mono_time_ns();
        let ts2 = mono_time_ns();
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
        let ts0 = mono_time_ns();

        let (seq_no, ts1) = if true {
            // This path performs a full payload copy.
            // In practice, the optimizer will only copy the bytes that are actually
            // used. In this example, only the `timestamp` field (8 bytes) is copied
            // out of the payload, not the entire payload buffer.
            let (seq_no, payload) = rx.recv();
            (seq_no, payload.timestamp)
        } else {
            // This path avoids copying the payload entirely by reading it directly
            // from the ring buffer.
            // `peek_unsafe` returns a direct reference to the payload inside the slot.
            // `advance_checked` verifies that the slot was not overwritten after the
            // read; only then is the payload considered valid and the loop can exit.
            loop {
                let (seq_no, payload) = unsafe { rx.peek_unsafe() };
                let ts1 = payload.timestamp;
                if rx.advance_checked() {
                    break (seq_no, ts1);
                }
            }
        };

        let ts2 = mono_time_ns();
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
