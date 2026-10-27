mod auth;
#[cfg(feature = "cast")]
mod cast;
mod cli;
mod client;
mod command;
mod config;
mod event;
#[allow(unsafe_code)] // loads fzt's C library
mod fzt;
#[cfg(feature = "streaming")]
mod handover;
#[cfg(any(feature = "cast", feature = "streaming"))]
mod home_auth;
#[cfg(feature = "streaming")]
mod keeper;
mod key;
#[cfg(feature = "streaming")]
mod listeners;
mod log_layer;
#[cfg(feature = "media-control")]
mod media_control;
#[cfg(feature = "streaming")]
mod play_report;
mod playlist_folders;
#[cfg(any(feature = "cast", feature = "streaming"))]
mod relay;
#[cfg(any(feature = "cast", feature = "streaming"))]
mod screen;
mod state;
#[cfg(feature = "streaming")]
mod streaming;
mod token;
mod ui;
mod utils;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use std::{collections::VecDeque, io::Write, sync::Arc};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::config::apply_config_override;

fn log_folder() -> &'static std::path::Path {
    config::get_config()
        .app_config
        .log_folder
        .as_deref()
        .expect("log_folder is set")
}

fn init_logging(
    log_folder: &std::path::Path,
    log_buffer: Arc<Mutex<VecDeque<String>>>,
) -> Result<()> {
    if std::env::var_os("RUST_LOG").is_some_and(|x| x == "off") {
        // Don't create log files if logging is disabled.
        return Ok(());
    }

    let log_prefix = format!(
        "spotify-player-{}",
        chrono::Local::now().format("%y-%m-%d-%H-%M")
    );

    // initialize the application's logging
    if std::env::var("RUST_LOG").is_err() {
        // default to log the current crate and librespot crates
        std::env::set_var("RUST_LOG", "spotify_player=info,librespot=info");
    }
    if !log_folder.exists() {
        std::fs::create_dir_all(log_folder)?;
    }
    let log_file = std::fs::File::create(log_folder.join(format!("{log_prefix}.log")))
        .context("failed to create log file")?;

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(log_file));

    let buffer_layer = crate::log_layer::BufferLayer::new(log_buffer, 1000);

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(fmt_layer)
        .with(buffer_layer)
        .init();

    // initialize the application's panic backtrace
    let backtrace_file = std::fs::File::create(log_folder.join(format!("{log_prefix}.backtrace")))
        .context("failed to create backtrace file")?;
    let backtrace_file = std::sync::Mutex::new(backtrace_file);
    std::panic::set_hook(Box::new(move |info| {
        let mut file = backtrace_file.lock().unwrap();
        let backtrace = backtrace::Backtrace::new();
        writeln!(&mut file, "Got a panic: {info:#?}\n").unwrap();
        writeln!(&mut file, "Stack backtrace:\n{backtrace:?}").unwrap();
    }));

    Ok(())
}

#[tokio::main]
async fn start_app(state: &state::SharedState) -> Result<()> {
    // client channels
    let (client_pub, client_sub) = flume::unbounded::<client::ClientRequest>();

    // create a Spotify API client: a PC's goes through the session and never signs in
    #[cfg(feature = "cast")]
    let client = if state.follows_session() {
        client::AppClient::relayed().context("construct the relayed client")?
    } else {
        client::AppClient::new()
            .await
            .context("construct app client")?
    };
    #[cfg(not(feature = "cast"))]
    let client = client::AppClient::new()
        .await
        .context("construct app client")?;
    if !state.follows_session() {
        client
            .new_session(Some(state), true)
            .await
            .context("initialize new Spotify session")?;
    }

    // request user data
    client_pub.send(client::ClientRequest::GetCurrentUser)?;
    client_pub.send(client::ClientRequest::GetUserPlaylists)?;
    client_pub.send(client::ClientRequest::GetUserFollowedArtists)?;
    client_pub.send(client::ClientRequest::GetUserSavedAlbums)?;
    client_pub.send(client::ClientRequest::GetContext(state::ContextId::Tracks(
        state::USER_LIKED_TRACKS_ID.to_owned(),
    )))?;
    client_pub.send(client::ClientRequest::GetUserSavedShows)?;

    // client socket task (for handling CLI commands)
    tokio::task::spawn({
        let client = client.clone();
        let state = state.clone();
        async move {
            cli::start_socket(&client, Some(&state), None).await;
        }
    });

    // client event handler task
    tokio::task::spawn({
        let state = state.clone();
        let client = client.clone();
        async move {
            client::start_client_handler(&state, &client, &client_sub).await;
        }
    });

    // background task that detects an invalidated session and reconnects,
    // independent of any incoming client request
    if !state.follows_session() {
        tokio::task::spawn({
            let state = state.clone();
            let client = client.clone();
            async move {
                client::start_session_watcher(state, client).await;
            }
        });
    }

    #[cfg(feature = "streaming")]
    if state.is_session {
        tokio::task::spawn(listeners::watch(client.clone()));
        tokio::task::spawn(handover::watch_while_idle(client.clone(), state.clone()));
        // beside the shared session, so both slots keep the same screen
        tokio::task::spawn(screen::session::serve(
            config::get_config().session_folder().join("screen.json"),
            state.clone(),
            client.clone(),
        ));
        tokio::task::spawn(relay::session::serve(client.clone()));
    }

    // player event watcher task
    std::thread::Builder::new()
        .name("player-event-watcher".to_string())
        .spawn({
            let state = state.clone();
            let client_pub = client_pub.clone();
            move || {
                client::start_player_event_watcher(&state, &client_pub);
            }
        })?;

    if !state.is_session {
        #[cfg(feature = "cast")]
        cast::start(state.vis_bands.clone()).context("attach to the session")?;
        #[cfg(feature = "cast")]
        screen::pc::start(state, &client_pub).context("follow the shared screen")?;

        #[cfg(feature = "image")]
        ui::init_image_picker(state).context("initialize image picker")?;
        let terminal = ui::init_terminal().context("initialize terminal")?;

        // terminal event handler task
        std::thread::Builder::new()
            .name("terminal-event-handler".to_string())
            .spawn({
                let client_pub = client_pub.clone();
                let state = state.clone();
                move || {
                    event::start_event_handler(&state, &client_pub);
                }
            })?;

        // application UI task
        std::thread::Builder::new().name("ui".to_string()).spawn({
            let state = state.clone();
            move || ui::run(&state, terminal)
        })?;
    }

    #[cfg(feature = "media-control")]
    if config::get_config().app_config.enable_media_control {
        // media control task
        std::thread::Builder::new()
            .name("media-control".to_string())
            .spawn({
                let state = state.clone();
                move || {
                    if let Err(err) = media_control::start_event_watcher(&state, client_pub) {
                        tracing::error!(
                            "Failed to start the application's media control event watcher: err={err:#?}"
                        );
                    }
                }
            })?;

        // the winit's event loop must be run in the main thread
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            // Start an event loop that listens to OS window events.
            //
            // MacOS and Windows require an open window to be able to listen to media
            // control events. The below code will create an invisible window on startup
            // to listen to such events.
            let event_loop = winit::event_loop::EventLoop::new()?;
            #[allow(deprecated)]
            event_loop.run(move |_, _| {})?;
        }
    }

    // infinite loop to keep the main thread alive
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// Start logging for the TUI or the session, keeping recent lines for the logs page
fn init_app_logging() -> Result<Arc<Mutex<VecDeque<String>>>> {
    let log_buffer: Arc<Mutex<VecDeque<String>>> =
        Arc::new(Mutex::new(VecDeque::with_capacity(1000)));

    init_logging(log_folder(), log_buffer.clone())
        .context("failed to initialize application's logging")?;

    // log the application's configurations
    tracing::info!("Configurations: {:?}", config::get_config());
    Ok(log_buffer)
}

/// Run the TUI, or with `is_session` the home cast's headless session
fn run_app(is_session: bool, log_buffer: Arc<Mutex<VecDeque<String>>>) -> Result<()> {
    let state = std::sync::Arc::new(state::State::new(is_session, log_buffer));
    client::show_saved_radio_page(&state);
    start_app(&state)
}

fn main() -> Result<()> {
    // librespot depends on hyper-rustls which requires a crypto provider to be set up.
    // TODO: see if this can be fixed upstream
    rustls::crypto::ring::default_provider()
        .install_default()
        .unwrap();

    // parse command line arguments
    let args = cli::init_cli()?.get_matches();

    // initialize the application's cache and config folders
    let config_folder: std::path::PathBuf = args
        .get_one::<String>("config-folder")
        .expect("config-folder should have default value")
        .into();
    if !config_folder.exists() {
        std::fs::create_dir_all(&config_folder)?;
    }

    let cache_folder: std::path::PathBuf = args
        .get_one::<String>("cache-folder")
        .expect("cache-folder should have a default value")
        .into();
    let cache_audio_folder = cache_folder.join("audio");
    if !cache_audio_folder.exists() {
        std::fs::create_dir_all(&cache_audio_folder)?;
    }
    let cache_image_folder = cache_folder.join("image");
    if !cache_image_folder.exists() {
        std::fs::create_dir_all(&cache_image_folder)?;
    }

    // initialize the application configs
    {
        let mut configs = config::Configs::new(&config_folder, &cache_folder)?;
        if configs.app_config.log_folder.is_none() {
            // set the log folder to be the cache folder if it is not set
            configs.app_config.log_folder = Some(cache_folder);
        }
        if let Some(overrides) = args.get_many::<String>("config-override") {
            for override_str in overrides {
                let (key, value) = override_str.split_once('=').context(format!(
                    "Invalid override format: '{override_str}'. Expected KEY=VALUE"
                ))?;

                apply_config_override(&mut configs.app_config, key, value)?;
            }
        }
        config::set_config(configs);
    }

    match args.subcommand() {
        None => run_app(false, init_app_logging()?),
        #[cfg(feature = "streaming")]
        Some(("session", args)) => {
            let log_buffer = init_app_logging()?;
            if let Some(slots) = args.get_one::<String>("slots") {
                let release = args
                    .get_one::<String>("release")
                    .expect("required with --slots");
                handover::claim(std::path::Path::new(slots), release)?;
            }
            run_app(true, log_buffer)
        }
        Some((cmd, args)) => cli::handle_cli_subcommand(cmd, args),
    }
}
