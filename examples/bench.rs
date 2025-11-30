use hft_channel::{Trials, mono_time_ns, spmc_broadcast::channel};

#[cfg(not(unix))]
compile_error!("This crate only supports Unix-like operating systems.");

const BUFFER_LEN: usize = 4096;
const PAYLOAD_SIZE: usize = 248;
const TRIALS: usize = 100_000;

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

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
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

#[inline(never)]
fn writer() -> std::io::Result<()> {
    let mut trials = Trials::with_capacity(TRIALS);
    let mut trials2 = Trials::with_capacity(TRIALS);

    let mut payload = Payload::<PAYLOAD_SIZE>::default();

    let (tx, _) = channel("/test", BUFFER_LEN)?;    

    loop {
        let ts0 = mono_time_ns();
        payload.timestamp = ts0;
        tx.send_from(&payload);
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

#[inline(never)]
fn reader() -> std::io::Result<()> {
    let mut trials = Trials::with_capacity(TRIALS);
    let mut trials2 = Trials::with_capacity(TRIALS);

    let (_, mut rx) = channel::<Payload<PAYLOAD_SIZE>>("/test", BUFFER_LEN)?;

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
            // This path avoids copying the payload by reading it directly
            // from the ring buffer.
            // `peek_unsafe` returns a direct reference to the payload inside the slot.
            // `advance_checked` verifies that the slot was not overwritten after the
            // read; only then is the payload considered valid and the loop can exit.
            loop {
                let (seq_no, payload) = unsafe { rx.peek_unsafe() };
                let ts1 = payload.timestamp;
                if rx.advance() {
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
