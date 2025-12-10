use clap::{Parser, Subcommand};
use crossbeam_utils::CachePadded;
use hft_channel::{
    Trials, mono_time_ns,
    spmc_broadcast::{Buffer, Message, Receiver, Sender, channel, local_channel},
};
use std::io::Write;
use std::{
    fs::{File, OpenOptions},
    io::BufWriter,
    path::PathBuf,
};

#[cfg(not(unix))]
compile_error!("This crate only supports Unix-like operating systems.");

const DEFAULT_N_TRIALS: usize = 100_000;
const DEFAULT_BUFFER_LENGTH: usize = 1 << 16;
const PAYLOAD_SIZE: usize = 392;

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

    /// Number of messages to process.
    #[arg(short = 't', long = "trials", default_value_t = DEFAULT_N_TRIALS, value_name = "COUNT")]
    n_trials: usize,

    /// Channel buffer length (in slots).
    #[arg(short = 'l', long = "buffer-length", default_value_t = DEFAULT_BUFFER_LENGTH, value_name = "LENGTH")]
    buffer_length: usize,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Sends messages to a shared-memory channel.
    Writer {
        /// Minimum period between messages in nanoseconds; 0 disables throttling.
        #[arg(short = 'p', long = "period", default_value_t = 0)]
        period: u64,

        /// Output shared-memory segment name, which must begin with ‘/’.
        #[arg(short = 'o', long = "output", default_value = "/test-reader-writer")]
        output: String,
    },

    /// Receives messages from a shared-memory channel.
    Reader {
        /// Input shared-memory segment name, which must begin with ‘/’.
        #[arg(short = 'i', long = "input", default_value = "/test-reader-writer")]
        input: String,
    },

    /// Sends messages to a server via a shared-memory channel and
    /// receives responses on the server’s output channel.
    Client {
        /// Minimum period between messages in nanoseconds; 0 disables throttling.
        #[arg(short = 'p', long = "period", default_value_t = 0)]
        period: u64,

        /// Output shared-memory segment name, which must begin with ‘/’.
        #[arg(short = 'o', long = "output", default_value = "/test-client-server")]
        output: String,

        /// Input shared-memory segment name, which must begin with ‘/’.
        #[arg(short = 'i', long = "input", default_value = "/test-server-client")]
        input: String,
    },

    /// Receives messages on an input shared-memory channel and echoes
    /// them onto an output shared-memory channel.
    Server {
        /// Input shared-memory segment name, which must begin with ‘/’.
        #[arg(short = 'i', long = "input", default_value = "/test-client-server")]
        input: String,

        /// Output shared-memory segment name, which must begin with ‘/’.
        #[arg(short = 'o', long = "output", default_value = "/test-server-client")]
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

    /// Receives messages from a shared-memory channel and saves them to a file.
    Logger {
        /// Input shared-memory segment name, which must begin with ‘/’.
        #[arg(short = 'i', long = "input", default_value = "/test-reader-writer")]
        input: String,

        /// Output file path
        #[arg(short = 'f', long = "file", value_name = "PATH")]
        path: PathBuf,

        /// Logger buffer size in MBytes
        #[arg(
            short = 's',
            long = "buffer-size",
            default_value_t = 8,
            value_name = "SIZE"
        )]
        logger_buffer_size: usize,
    },
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();

    let buffer_length = args.buffer_length;
    let n_trials = args.n_trials;

    println!(
        "message size: {}, payload size: {}, cache padded payload size: {}, buffer length: {}, buffer size: {}, trials: {}",
        std::mem::size_of::<Message<Payload<PAYLOAD_SIZE>>>(),
        std::mem::size_of::<Payload<PAYLOAD_SIZE>>(),
        std::mem::size_of::<CachePadded<Payload<PAYLOAD_SIZE>>>(),
        buffer_length,
        std::mem::size_of::<Message<Payload<PAYLOAD_SIZE>>>() * buffer_length,
        n_trials,
    );

    match args.command {
        Commands::Writer { period, output } => {
            println!("period: {}, output: {:?}", period, output);
            let (tx, _) = channel(&output, buffer_length)?;
            writer(tx, period, n_trials)
        }

        Commands::Reader { input } => {
            println!("input: {:?}", input);
            let (_, rx) = channel(&input, buffer_length)?;
            reader(rx, n_trials)
        }

        Commands::Logger {
            input,
            path,
            logger_buffer_size,
        } => {
            let logger_buffer_size = logger_buffer_size << 20;
            println!(
                "input: {:?}, output file: {:?}, logger buffer size: {}",
                input, path, logger_buffer_size
            );
            let (_, rx) = channel(&input, buffer_length)?;
            let file = OpenOptions::new().create(true).append(true).open(&path)?;
            let writer = BufWriter::with_capacity(logger_buffer_size, file);

            logger(rx, writer, n_trials)
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
            let (tx1, _) = channel(output, buffer_length)?;
            let (_, rx2) = channel(input, buffer_length)?;
            client(tx1, rx2, period, n_trials)
        }

        Commands::Server { input, output } => {
            println!("output: {:?}, input: {:?}", output, input);
            let (_, rx1) = channel(input, buffer_length)?;
            let (tx2, _) = channel(output, buffer_length)?;
            server(tx2, rx1)
        }

        Commands::Broadcast {
            period,
            max_readers,
        } => {
            let (tx, rx) = local_channel(buffer_length);

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
                        reader(rx, n_trials)
                    })
                })
                .collect();

            let core_id = cores[1].clone();
            let writer = std::thread::spawn(move || {
                core_affinity::set_for_current(core_id);
                writer(tx, period, n_trials)
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

            let (tx1, rx1) = local_channel(buffer_length);
            let (tx2, rx2) = local_channel(buffer_length);

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
                client(tx1, rx2, period, n_trials)
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
    n_trials: usize,
) -> std::io::Result<()> {
    let mut trials: Trials<u64> = Trials::with_capacity(n_trials);
    let mut trials2: Trials<u64> = Trials::with_capacity(n_trials);

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

        if trials.len() == n_trials {
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
    n_trials: usize,
) -> std::io::Result<()> {
    let mut trials = Trials::with_capacity(n_trials);
    let mut trials2 = Trials::with_capacity(n_trials);
    let mut trials3 = Trials::with_capacity(n_trials);

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

        if trials.len() == n_trials {
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
    n_trials: usize,
) -> std::io::Result<()> {
    let mut trials = Trials::with_capacity(n_trials);
    let mut trials2 = Trials::with_capacity(n_trials);

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
        if trials.len() == n_trials {
            break;
        }
    }

    trials.sort();
    trials2.sort();
    trials.print_csv("B");
    trials2.print_csv("C");

    Ok(())
}

fn as_bytes<T: Sized>(p: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts((p as *const T) as *const u8, std::mem::size_of::<T>()) }
}

#[inline(never)]
fn logger(
    mut rx: Receiver<Payload<PAYLOAD_SIZE>, impl Buffer<Payload<PAYLOAD_SIZE>>>,
    mut writer: BufWriter<File>,
    n_trials: usize,
) -> std::io::Result<()> {
    let mut trials = Trials::with_capacity(n_trials);
    let mut trials2 = Trials::with_capacity(n_trials);
    let mut trials3 = Trials::with_capacity(n_trials);

    let mut prev_seq_no: u64 = 0;
    loop {
        let ts0 = mono_time_ns();

        let (seq_no, payload) = rx.recv();
        let ts1 = payload.timestamp;

        let ts2 = mono_time_ns();

        writer.write_all(as_bytes(payload))?;

        let ts3 = mono_time_ns();

        trials.push(ts2 - ts1);
        trials2.push(ts2 - ts0);
        trials3.push(ts3 - ts2);

        if prev_seq_no != 0 && prev_seq_no.wrapping_add(1) != seq_no {
            println!(
                "Skipped {} messages: prev seq_no {}, curr seq_no {}",
                seq_no - prev_seq_no,
                prev_seq_no,
                seq_no
            );
        }
        prev_seq_no = seq_no;
        if trials.len() == n_trials {
            break;
        }
    }

    trials.sort();
    trials2.sort();
    trials3.sort();
    trials.print_csv("B");
    trials2.print_csv("C");
    trials3.print_csv("W");

    Ok(())
}
