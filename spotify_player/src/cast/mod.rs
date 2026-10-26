//! The home cast, on a PC: spt attaches to the session on the NAS, receives its audio from the
//! Snapcast server there, and plays it in step with every other PC that has spt open. The
//! visualization is fed from what plays here, also when this PC is muted.

mod clock;
mod output;
mod protocol;
mod resample;
mod stream;

use std::io::BufReader;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Context as _;
use parking_lot::Mutex;

use crate::config;
use crate::ui::streaming::{Analyzer, VisBands};
use clock::Clock;
use output::Shared;
use protocol::Message;

/// Snapcast's port for audio clients, unless `cast_server` names another
const STREAM_PORT: u16 = 1704;
const RECONNECT_INTERVAL: Duration = Duration::from_secs(2);
const TIME_SYNC_INTERVAL: Duration = Duration::from_secs(1);
/// Time requests sent back to back after connecting, so the clock is right before the first chunk
const QUICK_SYNCS: u32 = 50;
/// How long to wait for a time response before asking again
const TIME_TIMEOUT: Duration = Duration::from_secs(2);
/// No audio for this long means nothing is playing
const SILENCE: Duration = Duration::from_millis(300);

struct Cast {
    muted: Arc<AtomicBool>,
    /// The connection's writing half, when connected
    writer: Mutex<Option<TcpStream>>,
}

static CAST: OnceLock<Cast> = OnceLock::new();

/// Attach to the session: connect to the cast server and play its audio, in the background
pub fn start(bands: Option<Arc<Mutex<VisBands>>>) -> anyhow::Result<()> {
    let server = config::get_config().app_config.cast_server.clone();
    let cast = CAST.get_or_init(|| Cast {
        muted: Arc::new(AtomicBool::new(false)),
        writer: Mutex::new(None),
    });
    let shared = Arc::new(Mutex::new(Shared::default()));
    let clock = Arc::new(Clock::default());

    let played = match bands {
        Some(bands) => {
            let (tx, rx) = flume::bounded(64);
            std::thread::Builder::new()
                .name("cast-visualization".to_string())
                .spawn(move || analyze(&bands, &rx))?;
            Some(tx)
        }
        None => None,
    };
    std::thread::Builder::new()
        .name("cast-output".to_string())
        .spawn({
            let shared = Arc::clone(&shared);
            let clock = Arc::clone(&clock);
            let muted = Arc::clone(&cast.muted);
            move || output::run(&shared, &clock, &muted, played.as_ref())
        })?;
    std::thread::Builder::new()
        .name("cast-connection".to_string())
        .spawn(move || loop {
            if let Err(err) = connect(cast, &server, &shared, &clock) {
                tracing::warn!("The cast connection to {server} ended: {err:#}");
            }
            *cast.writer.lock() = None;
            std::thread::sleep(RECONNECT_INTERVAL);
        })?;
    Ok(())
}

/// Whether this PC plays the session's audio aloud (rather than only showing it)
pub fn is_listening() -> bool {
    CAST.get()
        .is_some_and(|cast| !cast.muted.load(Ordering::Relaxed))
}

/// Switch between playing the session aloud here and only showing it. The session pauses when no
/// PC is left playing it aloud.
pub fn toggle_listening() {
    let Some(cast) = CAST.get() else {
        return;
    };
    let muted = !cast.muted.fetch_xor(true, Ordering::Relaxed);
    tracing::info!(
        "Cast: {}",
        if muted { "muted here" } else { "playing here" }
    );
    send_client_info(cast, muted);
}

fn send_client_info(cast: &Cast, muted: bool) {
    if let Some(writer) = cast.writer.lock().as_mut() {
        if let Err(err) = protocol::send(writer, &protocol::client_info(muted, clock::now())) {
            tracing::warn!("Failed to tell the cast server about muting: {err:#}");
        }
    }
}

fn connect(
    cast: &'static Cast,
    server: &str,
    shared: &Arc<Mutex<Shared>>,
    clock: &Arc<Clock>,
) -> anyhow::Result<()> {
    let (host, port) = match server.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().context("cast_server's port")?),
        None => (server, STREAM_PORT),
    };
    let addr = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("resolve {server}"))?
        .next()
        .with_context(|| format!("{server} has no address"))?;
    let socket = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .with_context(|| format!("connect to {addr}"))?;
    socket.set_nodelay(true)?;
    tracing::info!("Connected to the cast server at {addr}");

    let host = gethostname::gethostname().to_string_lossy().into_owned();
    let mut writer = socket.try_clone()?;
    protocol::send(&mut writer, &protocol::hello(&host, &host, clock::now()))?;
    // this PC's choice to play aloud or not outlasts reconnects; the server's stored one doesn't
    protocol::send(
        &mut writer,
        &protocol::client_info(cast.muted.load(Ordering::Relaxed), clock::now()),
    )?;
    *cast.writer.lock() = Some(writer.try_clone()?);
    clock.reset();
    shared.lock().stream.clear();

    let (answered, answers) = flume::bounded(1);
    std::thread::Builder::new()
        .name("cast-time-sync".to_string())
        .spawn(move || sync_time(&mut writer, &answers))?;
    // dropping `answered` when the connection ends stops the time sync
    receive(&mut BufReader::new(socket), cast, shared, clock, &answered)
}

/// Send time requests for as long as the connection lasts. Like snapclient, each one waits for the
/// answer to the one before, and the first for the server to answer the hello: a request that
/// waits in a queue on the server measures the queue, not the clocks.
fn sync_time(writer: &mut TcpStream, answers: &flume::Receiver<()>) {
    static NEXT_ID: AtomicU16 = AtomicU16::new(1);
    if answers.recv().is_err() {
        return;
    }
    let mut sent = 0u32;
    loop {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        if protocol::send(writer, &protocol::time_request(id, clock::now())).is_err() {
            return;
        }
        sent += 1;
        if let Err(flume::RecvTimeoutError::Disconnected) = answers.recv_timeout(TIME_TIMEOUT) {
            return;
        }
        if sent >= QUICK_SYNCS {
            std::thread::sleep(TIME_SYNC_INTERVAL);
        }
    }
}

fn receive(
    reader: &mut BufReader<TcpStream>,
    cast: &Cast,
    shared: &Arc<Mutex<Shared>>,
    clock: &Arc<Clock>,
    answered: &flume::Sender<()>,
) -> anyhow::Result<()> {
    // The server's answer to the hello carries the muting it stored, which this PC's own choice
    // replaces; any later settings come from another PC muting or unmuting this one.
    let mut hello_answered = false;
    loop {
        let (header, message) = protocol::read(reader, clock::now)?;
        match message {
            Message::ServerSettings(settings) => {
                tracing::info!("Cast server settings: {settings:?}");
                if hello_answered {
                    cast.muted.store(settings.muted, Ordering::Relaxed);
                }
                hello_answered = true;
                shared
                    .lock()
                    .set_buffer((settings.buffer_ms - settings.latency) * 1000);
                let _ = answered.try_send(());
            }
            Message::CodecHeader(format) => {
                tracing::info!("Cast stream format: {format:?}");
                anyhow::ensure!(
                    format.bits == 16 && usize::from(format.channels) == output::CHANNELS,
                    "the cast server must stream 16-bit stereo, not {format:?}"
                );
                shared.lock().set_input(format);
            }
            Message::WireChunk { timestamp, payload } => {
                if clock.is_synced() {
                    let now = clock.server_now();
                    shared.lock().push_chunk(timestamp, &payload, now);
                }
            }
            Message::Time { latency } => {
                clock.add(latency, header.received - header.sent);
                let _ = answered.try_send(());
            }
            Message::Error(err) => anyhow::bail!("the cast server refused: {err}"),
            Message::Other(kind) => tracing::debug!("Ignoring a cast message of type {kind}"),
        }
    }
}

/// Feed the visualization from what the sound card plays
fn analyze(bands: &Arc<Mutex<VisBands>>, played: &flume::Receiver<(u32, Vec<f32>)>) {
    let mut analyzer: Option<(u32, Analyzer)> = None;
    loop {
        match played.recv_timeout(SILENCE) {
            Ok((rate, samples)) => {
                if analyzer.as_ref().is_none_or(|(r, _)| *r != rate) {
                    analyzer = Some((rate, Analyzer::new(Arc::clone(bands), rate as f32)));
                }
                if let Some((_, analyzer)) = analyzer.as_mut() {
                    analyzer.push(&samples);
                }
            }
            Err(flume::RecvTimeoutError::Timeout) => {
                if let Some((_, mut analyzer)) = analyzer.take() {
                    analyzer.stop();
                }
            }
            Err(flume::RecvTimeoutError::Disconnected) => return,
        }
    }
}
