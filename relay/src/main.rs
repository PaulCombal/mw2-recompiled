//! mw2-relay: the server players choose with the launcher's NETWORK SERVER.
//! Everyone connected to the same relay sees each other's System Link games and
//! Xbox LIVE parties, and plays them through it: the relay hands every packet on
//! (protocol.rs), so the players need no open port and never learn each other's
//! address. It runs no match itself; whoever hosts one plays in it. It also keeps
//! the sessions they list for matchmaking (directory.rs), and serves the
//! playlists Find Game offers.
//!
//!   mw2-relay [--port N] [--password TEXT] [--max N] [--per-address N]
//!             [--playlists FILE] [--log-file FILE] [--status-port N]
//!
//! --port is the UDP port it listens on (3074), --password what a copy must
//! give to join (MW2_RELAY_PASSWORD in the environment, for a container), --max
//! how many copies it takes at once (64), --per-address how many of them from
//! one IP address (16: players behind one router share theirs). It keeps
//! nothing but the copies it took in, each forgotten when it leaves or goes
//! quiet, with what they listed. --playlists is the file every copy on the relay
//! gets, read once at the start; without one Find Game has nothing to offer.
//! It logs a line for each copy that joins and leaves, to the standard output
//! and to --log-file if given, and stops on SIGINT or SIGTERM. --status-port
//! answers any TCP connection with what the relay holds right now, as JSON, and
//! is meant for the machine's own use: Docker can publish it on 127.0.0.1 alone.

mod directory;
mod log;
mod protocol;
mod relay;

use log::log;
use protocol::*;
use relay::{Limits, Relay};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, UdpSocket};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How long a wait for a packet lasts: how late a stop or a status answer can be.
const WAIT: Duration = Duration::from_millis(100);

struct Options {
    port: u16,
    password: String,
    most: usize,
    per_address: usize,
    status_port: u16,
    playlists: Option<String>,
    log_file: Option<String>,
}

const USAGE: &str = "usage: mw2-relay [--port N] [--password TEXT] [--max N] [--per-address N]\n                 [--playlists FILE] [--log-file FILE] [--status-port N]";

fn options() -> Option<Options> {
    let mut options = Options {
        port: DEFAULT_PORT,
        password: std::env::var("MW2_RELAY_PASSWORD").unwrap_or_default(),
        most: 64,
        per_address: 16,
        status_port: 0,
        playlists: None,
        log_file: None,
    };
    let mut arguments = std::env::args().skip(1);
    while let Some(option) = arguments.next() {
        let value = arguments.next()?;
        match option.as_str() {
            "--port" => options.port = value.parse().ok().filter(|&port| port != 0)?,
            "--password" => options.password = value,
            "--max" => options.most = value.parse().ok().filter(|&most| most != 0)?,
            "--per-address" => options.per_address = value.parse().ok().filter(|&most| most != 0)?,
            "--playlists" => options.playlists = Some(value),
            "--log-file" => options.log_file = Some(value),
            "--status-port" => options.status_port = value.parse().ok()?,
            _ => return None,
        }
    }
    Some(options)
}

/// Takes one connection, says `text` and closes it. Waits on nobody: what the
/// socket will not take at once is dropped, so a client that reads nothing
/// cannot hold up the players.
fn answer_status(listening: &TcpListener, relay: &Relay) {
    while let Ok((mut client, _)) = listening.accept() {
        if client.set_nonblocking(true).is_err() {
            continue;
        }
        let answer = format!(
            "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
            relay.status()
        );
        let mut ignored = [0; 512];
        let _ = client.read(&mut ignored);
        let _ = client.write_all(answer.as_bytes());
        let _ = client.shutdown(Shutdown::Both);
    }
}

fn run(options: Options) -> Result<(), String> {
    let hello_password = size_of::<Hello>() - 4;
    if options.password.len() >= hello_password {
        return Err(format!("the password is longer than {} characters", hello_password - 1));
    }
    if let Some(path) = &options.log_file {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|error| format!("cannot write {path}: {error}"))?;
        log::to_file(file);
    }
    let mut playlists = Vec::new();
    if let Some(path) = &options.playlists {
        playlists = fs::read(path).map_err(|error| format!("cannot read {path}: {error}"))?;
        if playlists.is_empty() || playlists.len() > LARGEST_FILE {
            return Err(format!("{path} has {} bytes; it must have 1 to {LARGEST_FILE}", playlists.len()));
        }
    }

    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, options.port))
        .map_err(|error| format!("cannot listen on UDP port {}: {error}", options.port))?;
    socket.set_read_timeout(Some(WAIT)).map_err(|error| error.to_string())?;
    let receiving = socket.try_clone().map_err(|error| error.to_string())?;
    let mut status = None;
    if options.status_port != 0 {
        let listening = TcpListener::bind((Ipv4Addr::UNSPECIFIED, options.status_port))
            .map_err(|error| format!("cannot listen on TCP port {}: {error}", options.status_port))?;
        listening.set_nonblocking(true).map_err(|error| error.to_string())?;
        status = Some(listening);
    }
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(signal, Arc::clone(&stop)).map_err(|error| error.to_string())?;
    }
    log!(
        "mw2-relay, protocol {PROTOCOL}, on UDP port {}, {}, at most {} copies, {} from one address, playlists of {} bytes",
        options.port,
        if options.password.is_empty() { "no password" } else { "with a password" },
        options.most,
        options.per_address,
        playlists.len()
    );

    let limits = Limits { password: options.password, most: options.most, per_address: options.per_address };
    let mut relay = Relay::new(socket, limits, playlists);
    let mut packet = vec![0; LARGEST];
    let mut swept = Instant::now();
    let mut looked = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        match receiving.recv_from(&mut packet) {
            Ok((size, SocketAddr::V4(from))) => relay.heard(&packet[..size], from),
            Ok(_) => {}
            // The wait ran out, or a signal ended it; the loop's own checks follow.
            Err(_) => {}
        }
        let now = Instant::now();
        if let Some(listening) = &status
            && now.duration_since(looked) >= WAIT
        {
            looked = now;
            answer_status(listening, &relay);
        }
        if now.duration_since(swept) >= Duration::from_secs(1) {
            relay.sweep();
            swept = now;
        }
    }
    log!("stopped");
    Ok(())
}

fn main() -> ExitCode {
    let Some(options) = options() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    match run(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("mw2-relay: {why}");
            ExitCode::FAILURE
        }
    }
}
