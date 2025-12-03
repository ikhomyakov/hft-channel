use clap::{Parser, Subcommand};
use crossbeam_utils::CachePadded;
use hft_channel::{
    Trials, mono_time_ns,
    spmc_broadcast::{Buffer, Message, Receiver, Sender, channel, local_channel},
};

#[cfg(not(unix))]
compile_error!("This crate only supports Unix-like operating systems.");

const BUFFER_LEN: usize = 4096;
const PAYLOAD_SIZE: usize = 392;
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

#[derive(Parser, Debug)]
#[command(version, about = "Benchmarks for an SPMC broadcast channel", long_about = None)]
struct Args {
    /// Operation mode, selected via subcommand.
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Sends messages to a shared-memory channel.
    Writer {
        /// Minimum period between messages in nanoseconds; 0 disables throttling.
        #[arg(short = 'p', long = "period", default_value_t = 0)]
        period: u64,

        /// Output shared-memory segment name, which must begin with ‘/’.
        #[arg(short = 'o', long = "output", default_value = "test-reader-writer")]
        output: String,
    },

    /// Receives messages from a shared-memory channel.
    Reader {
        /// Input shared-memory segment name, which must begin with ‘/’.
        #[arg(short = 'i', long = "input", default_value = "test-reader-writer")]
        input: String,
    },

    /// Sends messages to a server via a shared-memory channel and
    /// receives responses on the server’s output channel.
    Client {
        /// Minimum period between messages in nanoseconds; 0 disables throttling.
        #[arg(short = 'p', long = "period", default_value_t = 0)]
        period: u64,

        /// Output shared-memory segment name, which must begin with ‘/’.
        #[arg(short = 'o', long = "output", default_value = "test-client-server")]
        output: String,

        /// Input shared-memory segment name, which must begin with ‘/’.
        #[arg(short = 'i', long = "input", default_value = "test-server-client")]
        input: String,
    },

    /// Receives messages on an input shared-memory channel and echoes
    /// them onto an output shared-memory channel.
    Server {
        /// Input shared-memory segment name, which must begin with ‘/’.
        #[arg(short = 'i', long = "input", default_value = "test-client-server")]
        input: String,

        /// Output shared-memory segment name, which must begin with ‘/’.
        #[arg(short = 'o', long = "output", default_value = "test-server-client")]
        output: String,
    },

    /// Runs one writer and multiple readers using an internal memory channel.
    Broadcast {
        /// Minimum period between messages in nanoseconds; 0 disables throttling.
        #[arg(short = 'p', long = "period", default_value_t = 0)]
        period: u64,

        /// Maximum number of readers.
        #[arg(short = 'm', long = "max-readers", default_value_t = 4)]
        max_readers: usize,
    },

    /// Runs a client and a server over a pair of internal memory channels.
    Ping {
        /// Minimum period between messages in nanoseconds; 0 disables throttling.
        #[arg(short = 'p', long = "period", default_value_t = 0)]
        period: u64,
    },
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();

    println!(
        "message size: {}, payload size: {}, cache padded payload size: {}, buffer length: {}, buffer size: {}, trials: {}",
        std::mem::size_of::<Message<Payload<PAYLOAD_SIZE>>>(),
        std::mem::size_of::<Payload<PAYLOAD_SIZE>>(),
        std::mem::size_of::<CachePadded<Payload<PAYLOAD_SIZE>>>(),
        BUFFER_LEN,
        std::mem::size_of::<Message<Payload<PAYLOAD_SIZE>>>() * BUFFER_LEN,
        TRIALS,
    );

    match args.command {
        Commands::Writer { period, output } => {
            println!("period: {}, output: {:?}", period, output);
            let (tx, _) = channel(&output, BUFFER_LEN)?;
            writer(tx, period)
        }

        Commands::Reader { input } => {
            println!("input: {:?}", input);
            let (_, rx) = channel(&input, BUFFER_LEN)?;
            reader(rx)
        }

        Commands::Client {
            input,
            output,
            period,
        } => {
            println!(
                "period: {}, output: {:?}, input: {:?}",
                period, output, input
            );
            let (tx1, _) = channel(output, BUFFER_LEN)?;
            let (_, rx2) = channel(input, BUFFER_LEN)?;
            client(tx1, rx2, period)
        }

        Commands::Server { input, output } => {
            println!("output: {:?}, input: {:?}", output, input);
            let (_, rx1) = channel(input, BUFFER_LEN)?;
            let (tx2, _) = channel(output, BUFFER_LEN)?;
            server(tx2, rx1)
        }

        Commands::Broadcast {
            period,
            max_readers,
        } => {
            let (tx, rx) = local_channel(BUFFER_LEN);

            let cores = core_affinity::get_core_ids().unwrap();
            assert!(
                cores.len() > 1,
                "At least 2 CPU cores are required (found {}).",
                cores.len()
            );

            println!(
                "period: {}, n_readers: {}",
                period,
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
                writer(tx, period)
            });

            readers
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect::<std::io::Result<Vec<_>>>()?;

            writer.join().unwrap()?;

            Ok(())
        }
        Commands::Ping { period } => {
            println!("period: {}", period);

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
                client(tx1, rx2, period)
            });

            client.join().unwrap()?;

            Ok(())
        }
    }
}

#[inline(never)]
fn client(
    tx: Sender<Payload<PAYLOAD_SIZE>, impl Buffer<Payload<PAYLOAD_SIZE>>>,
    mut rx: Receiver<Payload<PAYLOAD_SIZE>, impl Buffer<Payload<PAYLOAD_SIZE>>>,
    period: u64,
) -> std::io::Result<()> {
    let mut trials: Trials<u64> = Trials::with_capacity(TRIALS);
    let mut trials2: Trials<u64> = Trials::with_capacity(TRIALS);

    let mut payload = Payload::<PAYLOAD_SIZE>::default();

    loop {
        let ts0 = mono_time_ns();
        payload.timestamp = ts0;
        tx.send_from(&payload);
        let (_, payload) = rx.recv();
        let ts1 = payload.timestamp;
        let ts2 = mono_time_ns();
        trials.push(ts2 - ts1);

        let ts3 = delay(ts0 + period);
        trials2.push(ts3 - ts0);

        if trials.len() == TRIALS {
            break;
        }
    }

    trials.sort();
    trials2.sort();
    trials.print_csv("D");
    trials2.print_csv("Y");

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

/// Busy-waits until the current monotonic time reaches or exceeds `deadline_ns`.
#[inline(always)]
fn delay(deadline_ns: u64) -> u64 {
    loop {
        let ts = mono_time_ns();
        if ts >= deadline_ns {
            break ts;
        }
    }
}

#[inline(never)]
fn writer(
    tx: Sender<Payload<PAYLOAD_SIZE>, impl Buffer<Payload<PAYLOAD_SIZE>>>,
    period: u64,
) -> std::io::Result<()> {
    let mut trials = Trials::with_capacity(TRIALS);
    let mut trials2 = Trials::with_capacity(TRIALS);
    let mut trials3 = Trials::with_capacity(TRIALS);

    let mut payload = Payload::<PAYLOAD_SIZE>::default();

    loop {
        let ts0 = mono_time_ns();
        payload.timestamp = ts0;
        tx.send_from(&payload);
        let ts1 = mono_time_ns();
        let ts2 = mono_time_ns();
        trials.push(ts1 - ts0);
        trials2.push(ts2 - ts1);

        let ts3 = delay(ts0 + period);
        trials3.push(ts3 - ts0);

        if trials.len() == TRIALS {
            break;
        }
    }

    trials.sort();
    trials2.sort();
    trials3.sort();
    trials.print_csv("A");
    trials2.print_csv("X");
    trials3.print_csv("Y");

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
