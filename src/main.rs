#![deny(clippy::unwrap_used, clippy::pedantic)]
#![allow(
    clippy::module_name_repetitions,
    clippy::unused_self,
    clippy::unnested_or_patterns,
    clippy::match_same_arms,
    clippy::manual_let_else,
    clippy::needless_return,
    clippy::zero_sized_map_values,
    clippy::too_many_lines,
    clippy::match_single_binding,
    unused_macros
)]
use std::{ops::Sub, sync::mpsc::TryRecvError, time::Duration};

use anyhow::Result;
use clap::Parser;
use config::{Args, Command, ConfigFile};
use crossterm::event::{Event, KeyEvent};
use log::{error, info, trace, warn};
use mpd::{client::Client, commands::idle::IdleEvent};
use ratatui::{prelude::Backend, Terminal};
use ron::extensions::Extensions;
use ui::Level;

use crate::{config::Config, mpd::mpd_client::MpdClient, ui::Ui, utils::macros::try_ret};

mod config;
mod logging;
mod mpd;
mod state;
mod ui;
mod utils;

#[derive(Debug)]
pub enum AppEvent {
    UserInput(KeyEvent),
    Status(String, Level),
    Log(Vec<u8>),
    IdleEvent(IdleEvent),
    RequestStatusUpdate,
    RequestRender,
}

fn read_cfg(args: &Args) -> Result<Config> {
    let file = std::fs::File::open(&args.config)?;
    let read = std::io::BufReader::new(file);
    let res: ConfigFile = ron::de::from_reader(read)?;
    res.try_into()
}

fn main() -> Result<()> {
    let args = Args::parse();
    match &args.command {
        Some(Command::Config) => {
            println!(
                "{}",
                ron::ser::to_string_pretty(
                    &ConfigFile::default(),
                    ron::ser::PrettyConfig::default()
                        .depth_limit(3)
                        .struct_names(false)
                        .compact_arrays(true)
                        .extensions(
                            Extensions::IMPLICIT_SOME
                                | Extensions::UNWRAP_NEWTYPES
                                | Extensions::UNWRAP_VARIANT_NEWTYPES
                        ),
                )?
            );
            return Ok(());
        }
        None => {
            let (tx, rx) = std::sync::mpsc::channel::<AppEvent>();
            logging::init(tx.clone()).expect("Logger to initialize");

            let config = Box::leak(Box::new(match read_cfg(&args) {
                Ok(val) => val,
                Err(err) => {
                    warn!(err:?; "Using default config");
                    ConfigFile::default().try_into()?
                }
            }));

            try_ret!(tx.send(AppEvent::RequestRender), "Failed to render first frame");

            let mut client = try_ret!(
                Client::init(config.address, "command", true),
                "Failed to connect to mpd"
            );

            let album_art_disabled = config.ui.album_art_width_percent == 0;
            let display_image_warn = if !album_art_disabled && !utils::kitty::check_kitty_support()? {
                warn!("Album art is enabled but kitty image protocol is not supported by your terminal, disabling album art");
                config.ui.album_art_width_percent = 0;
                true
            } else {
                false
            };

            let terminal = try_ret!(ui::setup_terminal(), "Failed to setup terminal");
            let state = try_ret!(state::State::try_new(&mut client, config), "Failed to create app state");

            let mut render_loop = RenderLoop::new(tx.clone(), config);
            if state.status.state == mpd::commands::status::State::Play {
                render_loop.start()?;
            }

            let tx_clone = tx.clone();
            let mut ui = Ui::new(client, state.config);
            if display_image_warn {
                ui.display_message(
                    "Album art is enabled but kitty image protocol is not supported by your terminal, disabling album art"
                        .to_owned(),
                    Level::Warn,
                );
            }

            std::thread::Builder::new()
                .name("input poll".to_owned())
                .spawn(|| input_poll_task(tx_clone))?;
            let main_task = std::thread::Builder::new().name("main task".to_owned()).spawn(|| {
                main_task(
                    ui,
                    state,
                    rx,
                    try_ret!(
                        Client::init(config.address, "state", true),
                        "Failed to connect to mpd with state client"
                    ),
                    render_loop,
                    terminal,
                );
                Ok(())
            })?;

            let mut idle_client = try_ret!(
                Client::init(config.address, "idle", true),
                "Failed to connect to mpd with idle client"
            );
            idle_client.set_read_timeout(None)?;
            std::thread::Builder::new()
                .name("idle task".to_owned())
                .spawn(|| idle_task(idle_client, tx))?;

            let original_hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |panic| {
                crossterm::terminal::disable_raw_mode().expect("Disabling of raw mode to succeed");
                crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen)
                    .expect("Exit from alternate screen to succeed");
                original_hook(panic);
            }));

            info!(config:?; "Application initialized successfully");

            let _ = main_task.join().expect("Main task to not fail");
        }
    }

    Ok(())
}

fn main_task<B: Backend + std::io::Write>(
    mut ui: Ui<'static>,
    mut state: state::State,
    event_receiver: std::sync::mpsc::Receiver<AppEvent>,
    mut client: Client<'_>,
    mut render_loop: RenderLoop,
    mut terminal: Terminal<B>,
) {
    let mut render_wanted = false;
    let max_fps = 30f64;
    let min_frame_duration = Duration::from_secs_f64(1f64 / max_fps);
    let mut last_render = std::time::Instant::now().sub(Duration::from_secs(10));
    ui.before_show(&mut state).expect("Initial render init to succeed");

    loop {
        let now = std::time::Instant::now();
        std::thread::sleep(
            min_frame_duration
                .checked_sub(now - last_render)
                .unwrap_or(Duration::ZERO),
        );

        let event = if render_wanted {
            match event_receiver.recv_timeout(
                min_frame_duration
                    .checked_sub(now - last_render)
                    .unwrap_or(Duration::ZERO),
            ) {
                Ok(v) => Some(v),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => None,
            }
        } else {
            event_receiver.recv().ok()
        };

        if let Some(event) = event {
            match event {
                AppEvent::UserInput(key) => match ui.handle_key(key, &mut state) {
                    Ok(ui::KeyHandleResult::SkipRender) => continue,
                    Ok(ui::KeyHandleResult::Quit) => break,
                    Ok(ui::KeyHandleResult::RenderRequested) => {
                        render_wanted = true;
                    }
                    Err(err) => {
                        error!(err:?; "Key handler failed",);
                        render_wanted = true;
                    }
                },
                AppEvent::Status(message, level) => {
                    ui.display_message(message, level);
                    render_wanted = true;
                }
                AppEvent::Log(msg) => {
                    state.logs.push_back(msg);
                    if state.logs.len() > 1000 {
                        state.logs.pop_front();
                    }
                }
                AppEvent::IdleEvent(event) => {
                    if let Err(err) = handle_idle_event(event, &mut state, &mut client, &mut render_loop) {
                        error!(error:? = err, event:?; "Failed handle idle event");
                    }
                    render_wanted = true;
                }
                AppEvent::RequestStatusUpdate => {
                    match client.get_status() {
                        Ok(status) => state.status = status,
                        Err(err) => {
                            error!(err:?; "Unable to send render command from status update loop");
                        }
                    };
                    render_wanted = true;
                }
                AppEvent::RequestRender => {
                    render_wanted = true;
                }
            }
        }
        if render_wanted {
            let till_next_frame = min_frame_duration.saturating_sub(now.duration_since(last_render));
            if till_next_frame != Duration::ZERO {
                continue;
            }
            terminal
                .draw(|frame| {
                    if let Err(err) = ui.render(frame, &mut state) {
                        error!(error:? = err; "Failed to render a frame");
                    };
                })
                .expect("Expected render to succeed");
            last_render = now;
            render_wanted = false;
        }
    }

    ui::restore_terminal(&mut terminal).expect("Terminal restore to succeed");
}

fn handle_idle_event(
    event: IdleEvent,
    state: &mut state::State,
    client: &mut Client<'_>,
    render_loop: &mut RenderLoop,
) -> Result<()> {
    match event {
        IdleEvent::Mixer => state.status.volume = try_ret!(client.get_volume(), "Failed to get volume"),
        IdleEvent::Player => {
            state.current_song = try_ret!(client.get_current_song(), "Failed get current song");
            state.status = try_ret!(client.get_status(), "Failed get status");
            if let Some(current_song) = state
                .queue
                .as_ref()
                .and_then(|p| p.iter().find(|s| state.status.songid.is_some_and(|i| i == s.id)))
            {
                if !state.config.ui.album_art_width_percent != 0 {
                    state.album_art = try_ret!(
                        client.find_album_art(&current_song.file),
                        "Failed to get find album art"
                    )
                    .map(state::MyVec::new);
                }
            }

            if state.status.state == mpd::commands::status::State::Play {
                render_loop.start()?;
            } else {
                render_loop.stop()?;
            }
        }
        IdleEvent::Options => state.status = try_ret!(client.get_status(), "Failed to get status"),
        IdleEvent::Playlist => state.queue = try_ret!(client.playlist_info(), "Failed to get playlist"),
        // TODO: handle these events eventually ?
        IdleEvent::Database
        | IdleEvent::Update
        | IdleEvent::Output
        | IdleEvent::Partition
        | IdleEvent::Sticker
        | IdleEvent::Subscription
        | IdleEvent::Message
        | IdleEvent::Neighbor
        | IdleEvent::Mount
        | IdleEvent::StoredPlaylist => {
            warn!(event:?; "Received unhandled event")
        }
    };
    Ok(())
}

fn idle_task(mut idle_client: Client<'_>, sender: std::sync::mpsc::Sender<AppEvent>) {
    let mut error_count = 0;
    loop {
        let events = match idle_client.idle() {
            Ok(val) => val,
            Err(err) => {
                if error_count > 5 {
                    error!(err:?; "Unexpected error when receiving idle events");
                    break;
                }
                warn!(err:?; "Unexpected error when receiving idle events");
                error_count += 1;
                std::thread::sleep(Duration::from_secs(error_count));
                continue;
            }
        };

        for event in events {
            trace!(idle_event:? = event; "Received idle event");
            if let Err(err) = sender.send(AppEvent::IdleEvent(event)) {
                error!(error:? = err; "Failed to send app event");
            }
        }
    }
}

fn input_poll_task(user_input_tx: std::sync::mpsc::Sender<AppEvent>) {
    loop {
        match crossterm::event::poll(Duration::from_millis(250)) {
            Ok(true) => {
                let event = match crossterm::event::read() {
                    Ok(e) => e,
                    Err(err) => {
                        warn!(error:? = err; "Failed to read input event");
                        continue;
                    }
                };
                if let Event::Key(key) = event {
                    if let Err(err) = user_input_tx.send(AppEvent::UserInput(key)) {
                        error!(error:? = err; "Failed to send user input");
                    }
                }
            }
            Ok(_) => {}
            Err(e) => warn!(error:? = e; "Error when polling for event"),
        }
    }
}

enum LoopEvent {
    Start,
    Stop,
}

#[derive(Debug)]
struct RenderLoop {
    event_tx: Option<std::sync::mpsc::Sender<LoopEvent>>,
}

impl RenderLoop {
    fn new(render_sender: std::sync::mpsc::Sender<AppEvent>, config: &Config) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<LoopEvent>();

        // send stop event at the start to not start the loop immedietally
        if let Err(err) = tx.send(LoopEvent::Stop) {
            error!(error:? = err; "Failed to properly initialize status update loop");
        }

        let Some(update_interval) = config.status_update_interval_ms.map(Duration::from_millis) else {
            return Self { event_tx: None };
        };
        std::thread::spawn(move || {
            loop {
                match rx.try_recv() {
                    Ok(LoopEvent::Stop) => loop {
                        if let Ok(LoopEvent::Start) = rx.recv() {
                            break;
                        }
                    },
                    Err(TryRecvError::Disconnected) => {
                        error!("Render loop channel is disconnected");
                    }
                    Ok(LoopEvent::Start) | Err(TryRecvError::Empty) => {} // continue with the update loop
                }

                std::thread::sleep(update_interval);
                if let Err(err) = render_sender.send(AppEvent::RequestStatusUpdate) {
                    error!(error:? = err; "Failed to send status update request");
                }
            }
        });
        Self { event_tx: Some(tx) }
    }

    fn start(&mut self) -> Result<()> {
        if let Some(tx) = &self.event_tx {
            Ok(tx.send(LoopEvent::Start)?)
        } else {
            Ok(())
        }
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(tx) = &self.event_tx {
            Ok(tx.send(LoopEvent::Stop)?)
        } else {
            Ok(())
        }
    }
}
