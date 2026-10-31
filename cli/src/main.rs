use std::io;
use std::net::SocketAddr;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use airlynx_core::receive::{self, ReceiveEvent, ReceiverConfig};
use airlynx_core::{browse, DEFAULT_TCP_PORT, DISCOVERY_PORT, DeviceId, DeviceKind, Identity, Peer, Profile, human_size, scan, send};
use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "airlynx", version, about = "Send files to another device on the local network")]
struct Cli {
    /// Where the device id and favorites are stored (shared with the desktop app by default)
    #[arg(long, global = true)]
    config_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Wait for incoming files from your favorites
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
        /// Keep a connection open to every favorite, so that it can start sessions with this device even when it
        /// cannot reach it directly (for a device behind NAT or a VPN)
        #[arg(long)]
        reverse: bool,
    },
    /// Send one or more files to a favorite, or to an address
    Send {
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Favorite name or id (prefix) to send to
        #[arg(short, long)]
        device: Option<String>,
        /// Receiver as IP, host, or host:port (skips discovery)
        #[arg(short, long, conflicts_with = "device")]
        to: Option<String>,
        /// Seconds to wait for discovery replies
        #[arg(long, default_value_t = 2)]
        wait: u64,
    },
    /// List a folder in a favorite's save folder (the folder itself without a path)
    Browse {
        /// Path inside the save folder, with "/" between names
        #[arg(default_value = "")]
        path: String,
        #[command(flatten)]
        target: Target,
    },
    /// Download a file from a favorite's save folder
    Get {
        /// Path inside the save folder, with "/" between names
        path: String,
        /// Directory to save the file into
        #[arg(short, long, default_value = ".")]
        out: PathBuf,
        #[command(flatten)]
        target: Target,
    },
    /// List other AirLynx devices on the network
    Discover {
        /// Seconds to wait for replies
        #[arg(long, default_value_t = 2)]
        wait: u64,
    },
    /// Manage favorite devices
    #[command(subcommand)]
    Favorite(FavoriteCommand),
}

/// Which device a command talks to.
#[derive(Args)]
struct Target {
    /// Favorite name or id (prefix)
    #[arg(short, long)]
    device: Option<String>,
    /// Device as IP, host, or host:port (skips discovery)
    #[arg(short, long, conflicts_with = "device")]
    to: Option<String>,
    /// Seconds to wait for discovery replies
    #[arg(long, default_value_t = 2)]
    wait: u64,
}

impl Target {
    fn resolve(self, profile: &Profile, me: &Identity) -> io::Result<SocketAddr> {
        match self.to {
            Some(target) => send::resolve(&target),
            None => pick_receiver(profile, me, self.device.as_deref(), Duration::from_secs(self.wait)),
        }
    }
}

#[derive(Subcommand)]
enum FavoriteCommand {
    /// Show favorites
    List,
    /// Add a device found on the network, by name or id (prefix), or any device by its full id
    Add {
        device: String,
        /// Name to store when adding by full id
        #[arg(long)]
        name: Option<String>,
        /// Seconds to wait for discovery replies
        #[arg(long, default_value_t = 2)]
        wait: u64,
    },
    /// Show what is known about favorites: address, port, route, last seen and so on (all of them without a name)
    Info {
        /// Favorite name or id (prefix)
        device: Option<String>,
    },
    /// Remove a favorite, by name or id (prefix)
    Remove { device: String },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> io::Result<()> {
    let dir = cli
        .config_dir
        .or_else(|| dirs::config_dir().map(|d| d.join("AirLynx")))
        .ok_or_else(|| io::Error::other("no config directory; pass --config-dir"))?;
    let profile = Profile::load(&dir)?;
    let hostname = gethostname::gethostname().to_string_lossy().into_owned();
    let me = Identity::new(profile.id, profile.name.as_deref().unwrap_or_else(|| hostname.strip_suffix(".local").unwrap_or(&hostname)))
        .with_kind(DeviceKind::Desktop);

    match cli.command {
        Command::Receive { dir, port, name, reverse } => {
            let me = name.map_or(me.clone(), |n| Identity::new(me.id, &n).with_kind(me.kind));
            run_receive(ReceiverConfig {
                dir,
                port,
                identity: me,
                favorites: profile.favorites,
                reverse,
            })
        }
        Command::Send { files, device, to, wait } => run_send(&profile, &me, files, device, to, Duration::from_secs(wait)),
        Command::Browse { path, target } => {
            let addr = target.resolve(&profile, &me)?;
            let entries = browse::list(addr, &me, &path)?;
            if entries.is_empty() {
                println!("(empty)");
            }
            for e in entries {
                let size = if e.is_dir { String::new() } else { human_size(e.size) };
                println!("{:<10}{}{}", size, e.name, if e.is_dir { "/" } else { "" });
            }
            Ok(())
        }
        Command::Get { path, out, target } => {
            let addr = target.resolve(&profile, &me)?;
            let saved = browse::download(addr, &me, &path, &out, |_, _| {})?;
            println!("Saved {}", saved.display());
            Ok(())
        }
        Command::Discover { wait } => {
            let peers = scan(&me, &profile.favorites, Duration::from_secs(wait))?;
            if peers.is_empty() {
                println!("No devices found");
            }
            for p in peers {
                let favorite = if profile.favorites.contains(p.id) { "favorite" } else { "" };
                let mutual = if p.mutual { "has you as favorite" } else { "" };
                println!("{}  {}  {:<24} {favorite} {mutual}", p.id, p.addr, format!("\"{}\"", p.name));
            }
            Ok(())
        }
        Command::Favorite(FavoriteCommand::List) => {
            let list = profile.favorites.list();
            if list.is_empty() {
                println!("No favorites yet. Try `airlynx discover`, then `airlynx favorite add <name>`");
            }
            for f in list {
                println!("{}  \"{}\"", f.id, f.name);
            }
            Ok(())
        }
        Command::Favorite(FavoriteCommand::Add { device, name, wait }) => {
            if let Ok(id) = device.parse::<DeviceId>() {
                let name = name.unwrap_or_else(|| format!("device {}", id.short()));
                profile.favorites.add(id, &name, DeviceKind::Unknown)?;
                println!("Added \"{name}\" ({}) as a favorite", id.short());
                return Ok(());
            }
            let peers = scan(&me, &profile.favorites, Duration::from_secs(wait))?;
            let peer = pick(&peers, &device, |p| (p.id, &p.name))?;
            profile.favorites.add(peer.id, &peer.name, peer.kind)?;
            println!("Added \"{}\" ({}) as a favorite", peer.name, peer.id.short());
            if !peer.mutual {
                println!("They need to add this device too before files can be exchanged.");
            }
            Ok(())
        }
        Command::Favorite(FavoriteCommand::Info { device }) => {
            let list = profile.favorites.list();
            let chosen = match &device {
                Some(query) => vec![pick(&list, query, |f| (f.id, &f.name))?],
                None => list.iter().collect(),
            };
            if chosen.is_empty() {
                println!("No favorites yet. Try `airlynx discover`, then `airlynx favorite add <name>`");
            }
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
            for (i, favorite) in chosen.into_iter().enumerate() {
                if i > 0 {
                    println!();
                }
                let sighting = profile.favorites.sighting(favorite.id);
                for (label, value) in favorite.details(sighting.as_ref(), now) {
                    println!("{label:<20}{value}");
                }
            }
            Ok(())
        }
        Command::Favorite(FavoriteCommand::Remove { device }) => {
            let list = profile.favorites.list();
            let favorite = pick(&list, &device, |f| (f.id, &f.name))?;
            profile.favorites.remove(favorite.id)?;
            println!("Removed \"{}\"", favorite.name);
            Ok(())
        }
    }
}

/// Finds the single item whose name equals `query` (ignoring case) or whose id starts with it.
fn pick<'a, T>(items: &'a [T], query: &str, key: impl Fn(&T) -> (DeviceId, &String)) -> io::Result<&'a T> {
    let query = query.to_lowercase();
    let matches: Vec<&T> = items
        .iter()
        .filter(|item| {
            let (id, name) = key(item);
            name.to_lowercase() == query || id.to_string().starts_with(&query)
        })
        .collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => Err(io::Error::new(io::ErrorKind::NotFound, format!("no device matches {query:?}"))),
        _ => Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{query:?} matches several devices; use more of the id"))),
    }
}

fn run_receive(config: ReceiverConfig) -> io::Result<()> {
    let dir = config.dir.clone();
    let name = config.identity.name.clone();
    let handle = receive::start(config, |event| match event {
        ReceiveEvent::Refused { peer, id, name } => {
            eprintln!("[{peer}] refused \"{name}\" ({id}): not a favorite. Add it with `airlynx favorite add {id} --name \"{name}\"`")
        }
        ReceiveEvent::NotWritable { peer, error } => {
            eprintln!("[{peer}] refused: cannot write to the save folder ({error})")
        }
        ReceiveEvent::Connected { peer, files } => println!("[{peer}] receiving {files} file(s)"),
        ReceiveEvent::FileSaved { peer, path, size } => {
            println!("[{peer}] saved {} ({})", path.display(), human_size(size))
        }
        ReceiveEvent::FileRejected { peer, name, reason } => eprintln!("[{peer}] {name}: {reason}"),
        ReceiveEvent::Listed { peer, path } => println!("[{peer}] listed /{path}"),
        ReceiveEvent::Served { peer, path, size } => println!("[{peer}] served /{path} ({})", human_size(size)),
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

fn run_send(
    profile: &Profile,
    me: &Identity,
    files: Vec<PathBuf>,
    device: Option<String>,
    to: Option<String>,
    wait: Duration,
) -> io::Result<()> {
    let entries = send::prepare(files)?;
    let addr = match (to, device) {
        (Some(target), _) => send::resolve(&target)?,
        (None, device) => pick_receiver(profile, me, device.as_deref(), wait)?,
    };

    println!("Connecting to {addr}...");
    let mut last_file = usize::MAX;
    let report = send::send_files(addr, me, &entries, |p| {
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

/// Finds a favorite on the network: the named one, or the only one online.
fn pick_receiver(profile: &Profile, me: &Identity, device: Option<&str>, wait: Duration) -> io::Result<SocketAddr> {
    println!("Looking for favorites...");
    let favorites: Vec<Peer> = scan(me, &profile.favorites, wait)?
        .into_iter()
        .filter(|p| profile.favorites.contains(p.id))
        .collect();
    let peer = match device {
        Some(query) => pick(&favorites, query, |p| (p.id, &p.name))?,
        None => match favorites.as_slice() {
            [] => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "no favorite is online; add one with `airlynx favorite add <name>`, or pass --to <ip>",
                ));
            }
            [peer] => peer,
            many => {
                eprintln!("Several favorites are online, pick one with --device:");
                for p in many {
                    eprintln!("  \"{}\"  {}", p.name, p.addr);
                }
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "ambiguous receiver"));
            }
        },
    };
    if !peer.mutual {
        eprintln!("Note: \"{}\" has not added this device as a favorite yet, so it will probably refuse.", peer.name);
    }
    println!("Found \"{}\" at {}", peer.name, peer.addr);
    Ok(peer.addr)
}
