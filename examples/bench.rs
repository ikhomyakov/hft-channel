use crossbeam_utils::CachePadded;
use hft_channel::{
    Trials, mono_time_ns,
    spmc_broadcast::{Buffer, Message, Receiver, Sender, channel, local_channel},
};

#[cfg(not(unix))]
compile_error!("This crate only supports Unix-like operating systems.");

const BUFFER_LEN: usize = 4096;
const PAYLOAD_SIZE: usize = 400;
const TRIALS: usize = 100_000;

/// Example payload type used in each ring-buffer slot (wrapped in a `Message`).
#[derive(Clone, Copy, Debug)]
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
        eprintln!(
            "Usage: {} writer|reader|client|server|ping|broadcast [max_readers]",
            args[0]
        );
        std::process::exit(1);
    }

    println!(
        "Message size: {}, payload size: {}, cache padded payload size: {}, buffer length: {}, buffer size: {}",
        std::mem::size_of::<Message<Payload<PAYLOAD_SIZE>>>(),
        std::mem::size_of::<Payload<PAYLOAD_SIZE>>(),
        std::mem::size_of::<CachePadded<Payload<PAYLOAD_SIZE>>>(),
        BUFFER_LEN,
        std::mem::size_of::<Message<Payload<PAYLOAD_SIZE>>>() * BUFFER_LEN
    );

    match args[1].as_str() {
        "writer" => {
            let (tx, _) = channel("/test-reader-writer", BUFFER_LEN)?;
            writer(tx)
        }
        "reader" => {
            let (_, rx) = channel("/test-reader-writer", BUFFER_LEN)?;
            reader(rx)
        }
        "client" => {
            let (tx1, _) = channel("/test-client-server", BUFFER_LEN)?;
            let (_, rx2) = channel("/test-server-client", BUFFER_LEN)?;
            client(tx1, rx2)
        }
        "server" => {
            let (_, rx1) = channel("/test-client-server", BUFFER_LEN)?;
            let (tx2, _) = channel("/test-server-client", BUFFER_LEN)?;
            server(tx2, rx1)
        }
        "broadcast" => {
            let (tx, rx) = local_channel(BUFFER_LEN);

            let cores = core_affinity::get_core_ids().unwrap();
            assert!(
                cores.len() > 1,
                "At least 2 CPU cores are required (found {}).",
                cores.len()
            );

            let max_readers = if args.len() > 2 {
                args[2]
                    .parse()
                    .expect(&format!("failed to parse `max_readers` {:?}", args[2]))
            } else {
                2
            };

            println!(
                "Running 1 writer and {} readers",
                (cores.len() - 2).min(max_readers)
            );

            let readers: Vec<_> = (2..cores.len().min(2 + max_readers))
                .map(|i| {
                    let rx = rx.clone();
                    let core_id = cores[i].clone();
                    std::thread::spawn(move || {
                        core_affinity::set_for_current(core_id);
                        reader(rx)
                    })
                })
                .collect();

            let core_id = cores[1].clone();
            let writer = std::thread::spawn(move || {
                core_affinity::set_for_current(core_id);
                writer(tx)
            });

            readers
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect::<std::io::Result<Vec<_>>>()?;

            writer.join().unwrap()?;

            Ok(())
        }
        "ping" => {
            let (tx1, rx1) = local_channel(BUFFER_LEN);
            let (tx2, rx2) = local_channel(BUFFER_LEN);

            let cores = core_affinity::get_core_ids().unwrap();
            assert!(
                cores.len() > 2,
                "At least 3 CPU cores are required (found {}).",
                cores.len()
            );

            let core_id = cores[1].clone();
            let _ = std::thread::spawn(move || {
                core_affinity::set_for_current(core_id);
                server(tx2, rx1)
            });

            let core_id = cores[2].clone();
            let client = std::thread::spawn(move || {
                core_affinity::set_for_current(core_id);
                client(tx1, rx2)
            });

            client.join().unwrap()?;

            Ok(())
        }
        _ => {
            eprintln!(
                "Usage: {} writer|reader|client|server|ping|broadcast [max_readers]",
                args[0]
            );
            std::process::exit(1);
        }
    }
}

#[inline(never)]
fn client(
    tx: Sender<Payload<PAYLOAD_SIZE>, impl Buffer<Payload<PAYLOAD_SIZE>>>,
    mut rx: Receiver<Payload<PAYLOAD_SIZE>, impl Buffer<Payload<PAYLOAD_SIZE>>>,
) -> std::io::Result<()> {
    let mut trials = Trials::with_capacity(TRIALS);

    let mut payload = Payload::<PAYLOAD_SIZE>::default();

    loop {
        let ts0 = mono_time_ns();
        payload.timestamp = ts0;
        tx.send_from(&payload);
        let (_, payload) = rx.recv();
        let ts1 = payload.timestamp;
        let ts2 = mono_time_ns();
        trials.push(ts2 - ts1);
        if trials.len() == TRIALS {
            break;
        }
    }

    trials.sort();
    trials.print_csv("D");

    Ok(())
}

#[inline(never)]
fn server(
    tx: Sender<Payload<PAYLOAD_SIZE>, impl Buffer<Payload<PAYLOAD_SIZE>>>,
    mut rx: Receiver<Payload<PAYLOAD_SIZE>, impl Buffer<Payload<PAYLOAD_SIZE>>>,
) -> ! {
    loop {
        let (_, payload) = rx.recv();
        tx.send_from(&payload);
    }
}

#[inline(never)]
fn writer(
    tx: Sender<Payload<PAYLOAD_SIZE>, impl Buffer<Payload<PAYLOAD_SIZE>>>,
) -> std::io::Result<()> {
    let mut trials = Trials::with_capacity(TRIALS);
    let mut trials2 = Trials::with_capacity(TRIALS);

    let mut payload = Payload::<PAYLOAD_SIZE>::default();

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
fn reader(
    mut rx: Receiver<Payload<PAYLOAD_SIZE>, impl Buffer<Payload<PAYLOAD_SIZE>>>,
) -> std::io::Result<()> {
    let mut trials = Trials::with_capacity(TRIALS);
    let mut trials2 = Trials::with_capacity(TRIALS);

    let mut prev_seq_no: u64 = 0;
    loop {
        let ts0 = mono_time_ns();

        let (seq_no, ts1) = if false {
            // This path performs a full payload copy.
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
