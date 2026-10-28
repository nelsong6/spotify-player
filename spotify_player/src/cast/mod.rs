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
use output::Feed;
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
/// The outputs chosen on this PC to play besides the default, in the PC's own cache folder
const CHOSEN_OUTPUTS_FILE: &str = "outputs.json";
/// Whether play here is off on this PC, kept across restarts, in the PC's own cache folder
const PLAY_HERE_FILE: &str = "play_here_off.json";

/// Whether this PC's output is silent
#[derive(Default)]
pub(crate) struct Mute {
    /// Muted here, as the cast server records it
    muted: AtomicBool,
    /// The session follows something playing elsewhere, which stays silent here unless asked for
    following: AtomicBool,
    play_followed: AtomicBool,
}

impl Mute {
    pub(crate) fn silent(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
            || (self.following.load(Ordering::Relaxed)
                && !self.play_followed.load(Ordering::Relaxed))
    }
}

struct Cast {
    mute: Arc<Mute>,
    /// The connection's writing half, when connected
    writer: Mutex<Option<TcpStream>>,
    /// Tells the outputs thread which outputs to play besides the default
    outputs: flume::Sender<output::Event>,
    chosen: Mutex<Vec<String>>,
    default_output: Mutex<Option<String>>,
}

static CAST: OnceLock<Cast> = OnceLock::new();

/// Attach to the session: connect to the cast server and play its audio, in the background
pub fn start(bands: Option<Arc<Mutex<VisBands>>>) -> anyhow::Result<()> {
    let server = config::get_config().app_config.cast_server.clone();
    let (events, changes) = flume::unbounded();
    let cast = CAST.get_or_init(|| Cast {
        mute: Arc::new(Mute {
            muted: AtomicBool::new(load_cache_file(PLAY_HERE_FILE).unwrap_or(false)),
            ..Mute::default()
        }),
        writer: Mutex::new(None),
        outputs: events.clone(),
        chosen: Mutex::new(load_cache_file(CHOSEN_OUTPUTS_FILE).unwrap_or_default()),
        default_output: Mutex::new(None),
    });
    let feed = Arc::new(Mutex::new(Feed::default()));
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
            let feed = Arc::clone(&feed);
            let clock = Arc::clone(&clock);
            let mute = Arc::clone(&cast.mute);
            let chosen = cast.chosen.lock().clone();
            move || {
                output::run(
                    &feed,
                    &clock,
                    &mute,
                    played.as_ref(),
                    &events,
                    &changes,
                    &cast.default_output,
                    chosen,
                );
            }
        })?;
    std::thread::Builder::new()
        .name("cast-connection".to_string())
        .spawn(move || loop {
            if let Err(err) = connect(cast, &server, &feed, &clock) {
                tracing::warn!("The cast connection to {server} ended: {err:#}");
            }
            *cast.writer.lock() = None;
            std::thread::sleep(RECONNECT_INTERVAL);
        })?;
    Ok(())
}

/// Whether this PC plays the session's audio aloud (rather than only showing it)
pub fn is_listening() -> bool {
    CAST.get().is_some_and(|cast| !cast.mute.silent())
}

/// Switch between playing the session aloud here and only showing it. The session pauses when no
/// PC is left playing it aloud. While the session follows something playing elsewhere, this
/// switches playing that here.
pub fn toggle_listening() {
    let Some(cast) = CAST.get() else {
        return;
    };
    if cast.mute.following.load(Ordering::Relaxed) {
        let play = !cast.mute.play_followed.fetch_xor(true, Ordering::Relaxed);
        tracing::info!(
            "Cast: {} what the session follows",
            if play {
                "playing here"
            } else {
                "silent here for"
            }
        );
        if play && cast.mute.muted.load(Ordering::Relaxed) {
            set_muted(false);
        }
        return;
    }
    set_muted(!cast.mute.muted.load(Ordering::Relaxed));
}

/// How one of this PC's audio outputs plays the session
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OutputChoice {
    /// Windows' default output, which always plays it
    Default,
    /// Chosen to play it too
    On,
    Off,
}

/// The names of this PC's audio outputs
pub fn output_names() -> Vec<String> {
    output::names()
}

pub fn output_choice(name: &str) -> OutputChoice {
    let Some(cast) = CAST.get() else {
        return OutputChoice::Off;
    };
    if cast.default_output.lock().as_deref() == Some(name) {
        OutputChoice::Default
    } else if cast.chosen.lock().iter().any(|chosen| chosen == name) {
        OutputChoice::On
    } else {
        OutputChoice::Off
    }
}

/// Switch an output other than the default between playing the session and not; this PC
/// remembers the choice
pub fn toggle_output(name: &str) {
    let Some(cast) = CAST.get() else {
        return;
    };
    if output_choice(name) == OutputChoice::Default {
        return;
    }
    let mut chosen = cast.chosen.lock();
    if let Some(i) = chosen.iter().position(|chosen| chosen == name) {
        chosen.remove(i);
        tracing::info!("Cast: {name} no longer plays here");
    } else {
        chosen.push(name.to_string());
        tracing::info!("Cast: {name} plays here too");
    }
    let _ = cast.outputs.send(output::Event::Chosen(chosen.clone()));
    save_cache_file(CHOSEN_OUTPUTS_FILE, &*chosen);
}

fn load_cache_file<T: serde::de::DeserializeOwned>(name: &str) -> Option<T> {
    let path = config::get_config().cache_folder.join(name);
    match std::fs::read(&path) {
        Ok(json) => serde_json::from_slice(&json)
            .inspect_err(|err| tracing::warn!("Failed to read {}: {err:#}", path.display()))
            .ok(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            tracing::warn!("Failed to read {}: {err:#}", path.display());
            None
        }
    }
}

fn save_cache_file<T: serde::Serialize + ?Sized>(name: &str, value: &T) {
    let path = config::get_config().cache_folder.join(name);
    if let Err(err) = serde_json::to_vec(value)
        .map_err(anyhow::Error::from)
        .and_then(|json| Ok(std::fs::write(&path, json)?))
    {
        tracing::warn!("Failed to save {}: {err:#}", path.display());
    }
}

/// Quitting: turn play here off on the way out, so the session knows this PC left rather than
/// dropped its connection. This PC's own choice is kept for its next start.
pub fn leave() {
    if let Some(cast) = CAST.get() {
        send_client_info(cast, true);
    }
}

/// Mute or unmute this PC
pub fn set_muted(muted: bool) {
    let Some(cast) = CAST.get() else {
        return;
    };
    cast.mute.muted.store(muted, Ordering::Relaxed);
    save_cache_file(PLAY_HERE_FILE, &muted);
    tracing::info!(
        "Cast: {}",
        if muted {
            "play here off"
        } else {
            "play here on"
        }
    );
    send_client_info(cast, muted);
}

/// Whether the session follows something playing elsewhere; that plays silently here until asked
/// for, each time following starts
pub fn set_following(following: bool) {
    let Some(cast) = CAST.get() else {
        return;
    };
    if !cast.mute.following.swap(following, Ordering::Relaxed) && following {
        cast.mute.play_followed.store(false, Ordering::Relaxed);
    }
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
    feed: &Arc<Mutex<Feed>>,
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
        &protocol::client_info(cast.mute.muted.load(Ordering::Relaxed), clock::now()),
    )?;
    *cast.writer.lock() = Some(writer.try_clone()?);
    clock.reset();
    feed.lock().clear();

    let (answered, answers) = flume::bounded(1);
    std::thread::Builder::new()
        .name("cast-time-sync".to_string())
        .spawn(move || sync_time(&mut writer, &answers))?;
    // dropping `answered` when the connection ends stops the time sync
    receive(&mut BufReader::new(socket), cast, feed, clock, &answered)
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
    feed: &Arc<Mutex<Feed>>,
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
                    cast.mute.muted.store(settings.muted, Ordering::Relaxed);
                    save_cache_file(PLAY_HERE_FILE, &settings.muted);
                }
                hello_answered = true;
                feed.lock()
                    .set_buffer((settings.buffer_ms - settings.latency) * 1000);
                let _ = answered.try_send(());
            }
            Message::CodecHeader(format) => {
                tracing::info!("Cast stream format: {format:?}");
                anyhow::ensure!(
                    format.bits == 16 && usize::from(format.channels) == output::CHANNELS,
                    "the cast server must stream 16-bit stereo, not {format:?}"
                );
                feed.lock().set_input(format);
            }
            Message::WireChunk { timestamp, payload } => {
                if clock.is_synced() {
                    let now = clock.server_now();
                    feed.lock().push_chunk(timestamp, &payload, now);
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
