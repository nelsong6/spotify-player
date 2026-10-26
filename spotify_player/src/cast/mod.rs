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

/// Snapcast's port for audio clients
const STREAM_PORT: u16 = 1704;
const RECONNECT_INTERVAL: Duration = Duration::from_secs(2);
const TIME_SYNC_INTERVAL: Duration = Duration::from_secs(1);
/// Time requests sent quickly after connecting, so the clock is right before the first chunk
const QUICK_SYNCS: u32 = 50;
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
    let addr = (server, STREAM_PORT)
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

    let connected = Arc::new(AtomicBool::new(true));
    std::thread::Builder::new()
        .name("cast-time-sync".to_string())
        .spawn({
            let connected = Arc::clone(&connected);
            move || sync_time(&mut writer, &connected)
        })?;
    let result = receive(&mut BufReader::new(socket), shared, clock);
    connected.store(false, Ordering::Relaxed);
    result
}

/// Send time requests for as long as the connection lasts
fn sync_time(writer: &mut TcpStream, connected: &AtomicBool) {
    static NEXT_ID: AtomicU16 = AtomicU16::new(1);
    let mut sent = 0u32;
    while connected.load(Ordering::Relaxed) {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        if protocol::send(writer, &protocol::time_request(id, clock::now())).is_err() {
            return;
        }
        sent += 1;
        std::thread::sleep(if sent < QUICK_SYNCS {
            Duration::from_millis(20)
        } else {
            TIME_SYNC_INTERVAL
        });
    }
}

fn receive(
    reader: &mut BufReader<TcpStream>,
    shared: &Arc<Mutex<Shared>>,
    clock: &Arc<Clock>,
) -> anyhow::Result<()> {
    loop {
        let (header, message) = protocol::read(reader, clock::now)?;
        match message {
            Message::ServerSettings(settings) => {
                tracing::info!("Cast server settings: {settings:?}");
                shared
                    .lock()
                    .set_buffer((settings.buffer_ms - settings.latency) * 1000);
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
            Message::Time { latency } => clock.add(latency, header.received - header.sent),
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
