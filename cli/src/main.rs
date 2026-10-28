use std::io;
use std::net::SocketAddr;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use airlynx_core::receive::{self, ReceiveEvent, ReceiverConfig};
use airlynx_core::{DEFAULT_TCP_PORT, DISCOVERY_PORT, discover, human_size, send};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "airlynx", version, about = "Send files to another device on the local network")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Wait for incoming files
    Receive {
        /// Directory to save files into
        #[arg(short, long, default_value = "received")]
        dir: PathBuf,
        /// TCP port to listen on
        #[arg(short, long, default_value_t = DEFAULT_TCP_PORT)]
        port: u16,
        /// Name shown to senders (defaults to the hostname)
        #[arg(short, long)]
        name: Option<String>,
    },
    /// Send one or more files
    Send {
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Receiver as IP, host, or host:port (skips discovery)
        #[arg(short, long)]
        to: Option<String>,
        /// Seconds to wait for discovery replies
        #[arg(long, default_value_t = 2)]
        wait: u64,
    },
    /// List receivers on the network
    Discover {
        /// Seconds to wait for replies
        #[arg(long, default_value_t = 2)]
        wait: u64,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Receive { dir, port, name } => {
            let name = name.unwrap_or_else(|| gethostname::gethostname().to_string_lossy().into_owned());
            run_receive(ReceiverConfig { dir, port, name })
        }
        Command::Send { files, to, wait } => run_send(files, to, Duration::from_secs(wait)),
        Command::Discover { wait } => discover(Duration::from_secs(wait)).map(|peers| {
            if peers.is_empty() {
                println!("No receivers found");
            }
            for p in peers {
                println!("{}  \"{}\"", p.addr, p.name);
            }
        }),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_receive(config: ReceiverConfig) -> io::Result<()> {
    let dir = config.dir.clone();
    let name = config.name.clone();
    let handle = receive::start(config, |event| match event {
        ReceiveEvent::Connected { peer, files } => println!("[{peer}] receiving {files} file(s)"),
        ReceiveEvent::FileSaved { peer, path, size } => {
            println!("[{peer}] saved {} ({})", path.display(), human_size(size))
        }
        ReceiveEvent::FileRejected { peer, name, reason } => eprintln!("[{peer}] {name}: {reason}"),
        ReceiveEvent::SessionEnded { peer, error: None } => println!("[{peer}] done"),
        ReceiveEvent::SessionEnded { peer, error: Some(e) } => eprintln!("[{peer}] error: {e}"),
        ReceiveEvent::FileStarted { .. } | ReceiveEvent::FileProgress { .. } => {}
    })?;

    println!(
        "AirLynx receiver \"{name}\" listening on TCP {} (discovery on UDP {DISCOVERY_PORT})",
        handle.port()
    );
    println!("Saving files to {}", dir.canonicalize()?.display());
    loop {
        thread::park();
    }
}

fn run_send(files: Vec<PathBuf>, to: Option<String>, wait: Duration) -> io::Result<()> {
    let entries = send::prepare(files)?;
    let addr = match to {
        Some(target) => send::resolve(&target)?,
        None => pick_receiver(wait)?,
    };

    println!("Connecting to {addr}...");
    let mut last_file = usize::MAX;
    let report = send::send_files(addr, &entries, |p| {
        if p.file_sent == p.file_size && p.file_index != last_file {
            last_file = p.file_index;
            println!("  sent {} ({})", p.file_name, human_size(p.file_size));
        }
        ControlFlow::Continue(())
    })?;

    let secs = report.elapsed.as_secs_f64().max(0.001);
    println!(
        "Done: {} file(s), {} in {secs:.2}s ({}/s)",
        report.sent.len(),
        human_size(report.bytes),
        human_size((report.bytes as f64 / secs) as u64)
    );
    if !report.failed.is_empty() {
        return Err(io::Error::other(format!(
            "failed verification: {}",
            report.failed.join(", ")
        )));
    }
    Ok(())
}

fn pick_receiver(wait: Duration) -> io::Result<SocketAddr> {
    println!("Looking for receivers...");
    let peers = discover(wait)?;
    match peers.as_slice() {
        [] => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no receivers found; is `airlynx receive` running? You can also pass --to <ip>",
        )),
        [peer] => {
            println!("Found \"{}\" at {}", peer.name, peer.addr);
            Ok(peer.addr)
        }
        many => {
            eprintln!("Multiple receivers found, pick one with --to:");
            for p in many {
                eprintln!("  {}  \"{}\"", p.addr, p.name);
            }
            Err(io::Error::new(io::ErrorKind::InvalidInput, "ambiguous receiver"))
        }
    }
}
