use anyhow::{Context, Result};
use clap::Parser;
use etcetera::app_strategy::Xdg;
use gtk4::{CssProvider, Orientation, gio, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use std::{env, thread};
use std::borrow::Cow;
use std::mem::replace;
use etcetera::AppStrategy;

#[derive(Parser)]
struct Args {
    #[arg(long, short)]
    daemon: bool,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct Config {
    replacements: HashMap<String, String>,
}

fn socket_path() -> PathBuf {
    if let Ok(x) = env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(x).join("tts-overlay.sock")
    } else {
        PathBuf::from("/tmp").join(format!("tts-overlay-{}.sock", users::get_current_uid()))
    }
}

fn compile_replacements(replacements: &HashMap<String, String>) -> Vec<(Regex, String)> {
    let mut compiled: Vec<(Regex, String)> = vec![];

    for (pattern, replacement) in replacements {
        let new_pattern = format!(r"(\\)?(\b{}\b)", pattern);

        match Regex::new(&new_pattern) {
            Ok(re) => {
                compiled.push((re, replacement.to_string()));
            },
            Err(e) => {
                eprintln!("Failed to compile replacement '{}': {}", new_pattern, e);
            }
        }
    }

    compiled
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();

    let strategy = etcetera::choose_app_strategy(etcetera::AppStrategyArgs {
        top_level_domain: "dev".to_string(),
        author: "Junckes".to_string(),
        app_name: "tts-overlay".to_string(),
    })?;

    let config_dir = strategy.config_dir();

    if !config_dir.exists() {
        fs::create_dir_all(&config_dir)?;
    }

    let config_file_path = strategy.in_config_dir("config.toml");

    if !config_file_path.exists() {
        let config = Config{
            replacements: HashMap::new(),
        };

        fs::write(&config_file_path, toml::to_string(&config)?)?;
    }

    let config_contents = fs::read_to_string(&config_file_path)
        .with_context(|| "failed to read config file")?;

    let config: Config =
        toml::from_str(&config_contents).with_context(|| "failed to parse config file")?;

    println!("Read configuration from {}", config_file_path.display());

    if args.daemon {
        let replacements = compile_replacements(&config.replacements);

        run_daemon(replacements).await
    } else {
        run_ui().await
    }
}

// runs the daemon, possibly the worst piece of code i've ever written
// TODO: rewrite this in a less awful way
async fn run_daemon(replacements: Vec<(Regex, String)>) -> Result<()> {
    let sock = socket_path();

    if sock.exists() {
        let _ = fs::remove_file(&sock);
    }

    let listener = UnixListener::bind(&sock)
        .with_context(|| format!("failed to bind unix socket at {}", sock.to_string_lossy()))?;

    listener
        .set_nonblocking(true)
        .context("failed to set nonblocking on listener")?;

    println!("Daemon listening on: {}", sock.display());

    // I fucking hate this code, and it should probably be redone to not look so hideous
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    let listener_thread = thread::spawn(move || {
        loop {
            if !running_clone.load(Ordering::Relaxed) {
                break;
            }

            // TODO: rewrite this as to not put the CPU on a spinlock, the previous method worked but it caused CTRL+C to not properly exit
            match listener.accept() {
                Ok((stream, _addr)) => {
                    let replacements = replacements.clone();
                    thread::spawn(move || {
                        if let Err(e) = handle_client(stream, replacements) {
                            eprintln!("client handler error: {e:#}");
                        }
                    });
                }
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::WouldBlock {
                        thread::sleep(Duration::from_millis(50));
                        continue;
                    } else {
                        eprintln!("listener accept error: {e}");
                        thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                }
            }
        }

        println!("Accept loop exiting.");
    });

    println!("Daemon started");
    tokio::signal::ctrl_c().await?;
    println!("Stopping daemon...");

    running.store(false, Ordering::Relaxed);
    if let Err(e) = listener_thread.join() {
        eprintln!("Failed to join listener thread: {e:?}");
    }

    if sock.exists() {
        let _ = fs::remove_file(&sock);
    }

    println!("Daemon stopped");

    Ok(())
}

fn handle_client(s: UnixStream, replacements: Vec<(Regex, String)>) -> Result<()> {
    let mut buf = String::new();
    let mut reader = BufReader::new(&s);
    reader.read_line(&mut buf)?;
    let text = buf.trim().to_string();
    if text.is_empty() {
        return Ok(());
    }

    let to_play = process_replacements(&text, &replacements);

    println!("Daemon received: \"{}\" -> \"{}\"", text, to_play);
    tts_and_play(&to_play)
}

fn process_replacements(text: &str, replacements: &Vec<(Regex, String)>) -> String {
    let mut current_text = text.to_string();

    for (pattern, replacement) in replacements {
        current_text = pattern.replace_all(&current_text, |caps: &Captures| {
            if caps.get(1).is_some() {
                caps.get(2)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default()
            } else {
                replacement.clone()
            }
        }).to_string();
    }

    current_text
}

fn tts_and_play(text: &str) -> Result<()> {
    let tmp = env::temp_dir().join(format!("tts_overlay_tts_{}.wav", std::process::id()));

    let status = Command::new("espeak")
        .arg("-w")
        .arg(&tmp)
        .arg(text)
        .status()
        .with_context(|| "failed to run espeak; is it installed?")?;
    if !status.success() {
        anyhow::bail!("espeak returned nonzero status");
    }

    let status = Command::new("paplay").arg(&tmp).status().with_context(
        || "failed to run paplay; do you have PulseAudio or pipewire-pulse and paplay installed?",
    )?;
    if !status.success() {
        anyhow::bail!("paplay returned nonzero status");
    }

    let _ = fs::remove_file(&tmp);
    Ok(())
}

async fn run_ui() -> Result<()> {
    use gtk4::prelude::*;

    let sock = socket_path();

    if !sock.exists() {
        return Err(anyhow::anyhow!("Unix socket does not exist"));
    }

    let application = gtk4::Application::new(
        Some("dev.junckes.tts-overlay"),
        gio::ApplicationFlags::FLAGS_NONE,
    );

    application.connect_activate(move |app| {
        build_and_show_overlay(app);
    });

    application.run();
    Ok(())
}

const WINDOW_CSS: &str = "
window {
    background: transparent;
    /* Optional: Remove any potential borders/shadows */
    border: none;
    box-shadow: none;
}
";

fn build_and_show_overlay(app: &gtk4::Application) {
    use gtk4::prelude::*;

    let provider = CssProvider::new();
    provider.load_from_string(WINDOW_CSS);

    let window = gtk4::ApplicationWindow::new(app);
    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    // window.set_default_size(800, 200);
    window.set_default_width(800);
    window.set_decorated(false);
    window.set_modal(true);
    window.auto_exclusive_zone_enable();
    window.set_margin(Edge::Top, 0);
    window.set_margin(Edge::Right, 0);
    window.set_margin(Edge::Bottom, 0);
    window.set_margin(Edge::Left, 0);
    window.set_keyboard_mode(KeyboardMode::Exclusive);

    if let Some(display) = gtk4::gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    let vbox = gtk4::Box::new(Orientation::Vertical, 6);
    vbox.set_margin_top(20);
    vbox.set_margin_bottom(20);
    vbox.set_margin_start(20);
    vbox.set_margin_end(20);

    let entry = gtk4::Entry::new();
    entry.set_placeholder_text(Some("Type text and press Enter to speak..."));
    entry.set_hexpand(true);
    entry.set_vexpand(false);
    entry.set_activates_default(true);

    vbox.append(&entry);
    window.set_child(Some(&vbox));

    window.present();
    entry.grab_focus();

    let event_controller = gtk4::EventControllerKey::new();

    let entry_window = window.clone();
    let sock = socket_path();
    entry.connect_activate(move |e| {
        let text = e.text().to_string();
        if !text.trim().is_empty() {
            println!("text: {}", text.trim());
            match UnixStream::connect(&sock) {
                Ok(mut s) => {
                    let _ = s.write_all(format!("{}\n", text.trim()).as_bytes());
                }
                Err(e) => {
                    eprintln!("Failed to connect to socket: {}", e);
                }
            }
        }

        entry_window.close();
    });

    let event_window = window.clone();
    event_controller.connect_key_pressed(move |_, key, _, _| {
        use gtk4::gdk::Key;

        if key == Key::Escape {
            event_window.close();
            return glib::Propagation::Stop;
        }

        glib::Propagation::Proceed
    });

    window.add_controller(event_controller);
}
