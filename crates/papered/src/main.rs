mod cli;

use clap::{Parser, Subcommand};
use cli::{ConfigAction, LatticeAction, ModelAction};
use colored::Colorize;
use papered::client::DaemonClient;
use papered::error::PaperedError;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "papered")]
#[command(about = "Local-first paper knowledge engine for AI agents and researchers")]
#[command(version = env!("CARGO_PKG_VERSION"))]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the Papered background daemon
    Daemon {
        #[arg(long)]
        foreground: bool,
    },

    /// Stop the background daemon
    Stop,

    /// Open the Papered web UI in your browser (starts the daemon if needed)
    Ui,

    /// Add a paper to your thought space
    Add {
        path: PathBuf,
        #[arg(short, long)]
        r#type: Option<String>,
    },

    /// Batch import all supported files from a directory
    Batch {
        path: PathBuf,
        #[arg(short, long)]
        recursive: bool,
    },

    /// Explore papers in your thought space by semantic affinity
    Search {
        query: String,
        #[arg(short, long)]
        section: Option<String>,
        #[arg(short, long, default_value = "10")]
        limit: usize,
        #[arg(short, long, default_value = "0.1")]
        min_score: f32,
        #[arg(long)]
        search_method: Option<String>,
    },

    /// Find papers similar to a given paper
    Similar {
        paper_id: String,
        #[arg(short, long)]
        section: Option<String>,
        #[arg(short, long, default_value = "5")]
        limit: usize,
    },

    /// List all papers in your thought space
    List {
        #[arg(short, long, default_value = "20")]
        limit: usize,
        #[arg(short, long, default_value = "0")]
        offset: usize,
    },

    /// Show details of a specific paper
    Show { paper_id: String },

    /// Delete a paper from your thought space
    Delete { paper_id: String },

    /// Re-index a paper (or all papers)
    Reindex {
        paper_id: Option<String>,
        #[arg(long)]
        all: bool,
    },

    /// Export papers to a file or directory
    Export {
        #[arg(short, long, default_value = "full_papers")]
        target: String,
        #[arg(short, long, default_value = "markdown")]
        format: String,
        #[arg(short, long)]
        output: String,
        #[arg(long)]
        paper_id: Option<String>,
    },

    /// Show thought space statistics
    Stats,

    /// Show LLM call usage and latency metrics (reads the local database directly)
    Metrics,

    /// Configure papered settings
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },

    /// Import and sync papers from Lattice.
    #[command(
        name = "lattice",
        after_help = "Lattice is a trademark of the Lattice Project Contributors. Papered is not affiliated with or endorsed by Lattice."
    )]
    Lattice {
        #[command(subcommand)]
        action: LatticeAction,
    },

    /// Manage embedding models and related configuration
    Model {
        #[command(subcommand)]
        action: ModelAction,
    },

    /// Re-compress existing extracted images to save storage space
    OptimizeImages {
        #[arg(long)]
        dry_run: bool,
    },

    /// Reset the local Papered data directory (keeps config file by default)
    Reset {
        #[arg(long)]
        force: bool,
        #[arg(long)]
        all: bool,
    },
}

// ---------------------------------------------------------------------------
// Daemon lifecycle helpers
// ---------------------------------------------------------------------------

fn daemon_port() -> u16 {
    papered::routes::DAEMON_DEFAULT_PORT
}

fn find_daemon_port() -> u16 {
    let port_file = papered::routes::daemon_port_file();

    if port_file.exists()
        && let Ok(content) = std::fs::read_to_string(&port_file)
        && let Ok(port) = content.trim().parse::<u16>()
    {
        return port;
    }

    daemon_port()
}

fn daemon_binary_name() -> &'static str {
    if cfg!(windows) {
        "papered-daemon.exe"
    } else {
        "papered-daemon"
    }
}

fn find_daemon_binary() -> Option<PathBuf> {
    let name = daemon_binary_name();

    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(name);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    if let Ok(paths) = std::env::var("PATH") {
        let sep = if cfg!(windows) { ';' } else { ':' };
        for path in paths.split(sep) {
            let candidate = PathBuf::from(path).join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    for c in &[
        format!("target/release/{name}"),
        format!("target/debug/{name}"),
        format!("../target/release/{name}"),
        format!("../target/debug/{name}"),
    ] {
        let p = PathBuf::from(c);
        if p.exists() {
            return Some(p);
        }
    }

    None
}

/// Log file receiving the spawned daemon's stdout/stderr, so startup failures
/// (port conflicts, database lock, config errors) stay diagnosable without
/// rerunning the daemon in the foreground.
fn daemon_log_file() -> PathBuf {
    let base = papered::AppConfig::load()
        .map(|c| c.data_dir)
        .unwrap_or_else(|_| papered::routes::daemon_port_dir());
    base.join("logs").join("daemon.log")
}

/// Maximum size of `daemon.log` before it is rotated to `daemon.log.1` on the
/// next daemon start. With a single backup kept, total disk usage stays
/// bounded at ~2x this value.
const DAEMON_LOG_MAX_BYTES: u64 = 50 * 1024 * 1024;

/// Rotate `daemon.log` to `daemon.log.1` when it exceeds `max_bytes`, keeping
/// at most one backup. A missing file is a no-op; failures (e.g. permissions)
/// are non-fatal and reported as `false` so the caller can proceed to append
/// regardless.
fn rotate_daemon_log_if_oversized(path: &Path, max_bytes: u64) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > max_bytes => {
            // Append ".1" to the file name itself, not via with_extension, so
            // the backup is always "daemon.log.1" regardless of the original
            // name's extension.
            let mut backup = path.as_os_str().to_os_string();
            backup.push(".1");
            let backup = PathBuf::from(backup);
            // std::fs::rename does not overwrite an existing destination on
            // Windows; drop a stale backup first so we keep exactly one.
            let _ = std::fs::remove_file(&backup);
            std::fs::rename(path, backup).is_ok()
        }
        _ => false,
    }
}

/// Open the daemon log for appending, returning paired handles for stdout and
/// stderr. `None` when the log directory or file cannot be created.
fn daemon_log_stdio() -> Option<(std::fs::File, std::fs::File)> {
    let log_file = daemon_log_file();
    // Rotate before appending so the log stays bounded. Failure is non-fatal:
    // appending to the current file continues either way.
    let _ = rotate_daemon_log_if_oversized(&log_file, DAEMON_LOG_MAX_BYTES);
    let out = std::fs::create_dir_all(log_file.parent()?)
        .ok()
        .and_then(|_| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_file)
                .ok()
        })?;
    let err = out.try_clone().ok()?;
    Some((out, err))
}

fn spawn_daemon() -> papered::error::Result<()> {
    let daemon_bin = find_daemon_binary()
        .ok_or_else(|| papered::PaperedError::io_other("papered-daemon binary not found. Please build it (cargo build --bin papered-daemon) or start the daemon manually."))?;

    println!("{}", "Daemon not running. Starting it now...".yellow());

    let mut cmd = std::process::Command::new(&daemon_bin);
    cmd.envs(std::env::vars());
    cmd.stdin(std::process::Stdio::null());

    match daemon_log_stdio() {
        Some((out, err)) => {
            cmd.stdout(out).stderr(err);
        }
        None => {
            cmd.stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
        }
    }

    let _child = cmd.spawn().map_err(|e| {
        papered::PaperedError::io_other(format!(
            "Failed to start papered-daemon at {}: {}",
            daemon_bin.display(),
            e
        ))
    })?;
    Ok(())
}

async fn ensure_daemon_running() -> papered::error::Result<DaemonClient> {
    let port = find_daemon_port();
    let client = DaemonClient::new(format!("http://127.0.0.1:{port}"))?;
    if client.health().await.unwrap_or(false) {
        return Ok(client);
    }

    // No healthy daemon. A live PID file means a daemon is starting up: it
    // registers its PID at process start but binds the HTTP port (and writes
    // the port file) only after initialization. Never spawn a second daemon
    // in that state — it would just fail on the database lock.
    if papered::util::process::running_daemon_pid().is_some() {
        println!(
            "{}",
            "Daemon is starting up; waiting for it to become ready...".yellow()
        );
    }

    // Re-resolve the port on every poll: the daemon writes the port file only
    // after binding, and it may land on a non-default port if 9321 is taken.
    let mut spawned = false;
    let mut dead_after_spawn = 0u32;
    for _ in 0..180 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let port = find_daemon_port();
        let client = DaemonClient::new(format!("http://127.0.0.1:{port}"))?;
        if client.health().await.unwrap_or(false) {
            println!("{}", "Daemon started successfully.".green());
            return Ok(client);
        }
        if papered::util::process::running_daemon_pid().is_none() {
            if spawned {
                // The daemon we spawned died before becoming healthy. The
                // PID file may lag the process start by a few polls, so only
                // give up after a sustained absence — retrying the spawn
                // would loop on the same failure.
                dead_after_spawn += 1;
                if dead_after_spawn >= 20 {
                    break;
                }
            } else {
                // No daemon running: clear stale registration files left by an
                // ungraceful exit and start one.
                let _ = std::fs::remove_file(papered::routes::daemon_port_file());
                let _ = std::fs::remove_file(papered::routes::daemon_pid_file());
                spawn_daemon()?;
                spawned = true;
            }
        }
    }

    Err(PaperedError::io_other(format!(
        "Timed out waiting for daemon to start. Check the daemon log at {}",
        daemon_log_file().display()
    )))
}

fn ui_url(base_url: &str) -> String {
    format!("{}/ui/", base_url.trim_end_matches('/'))
}

#[cfg(target_os = "macos")]
fn browser_opener(url: &str) -> (&'static str, Vec<String>) {
    ("open", vec![url.to_string()])
}

#[cfg(target_os = "linux")]
fn browser_opener(url: &str) -> (&'static str, Vec<String>) {
    ("xdg-open", vec![url.to_string()])
}

#[cfg(target_os = "windows")]
fn browser_opener(url: &str) -> (&'static str, Vec<String>) {
    (
        "cmd",
        vec!["/c".into(), "start".into(), "".into(), url.to_string()],
    )
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn browser_opener(url: &str) -> (&'static str, Vec<String>) {
    ("xdg-open", vec![url.to_string()])
}

fn open_browser(url: &str) {
    let (program, args) = browser_opener(url);
    if let Err(e) = std::process::Command::new(program).args(&args).status() {
        eprintln!("Could not launch a browser ({e}). Open this URL manually: {url}");
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    if let Err(e) = async_main().await {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

async fn async_main() -> papered::error::Result<()> {
    let cli = Cli::parse();

    if let Commands::Daemon { foreground } = cli.command {
        let daemon_bin = find_daemon_binary()
            .ok_or_else(|| papered::PaperedError::io_other("papered-daemon binary not found."))?;

        let mut cmd = std::process::Command::new(&daemon_bin);
        cmd.envs(std::env::vars());
        if !foreground {
            cmd.stdin(std::process::Stdio::null());
            // A backgrounded daemon outlives this terminal — send its output
            // to the daemon log instead of inheriting (and losing) our stdout.
            match daemon_log_stdio() {
                Some((out, err)) => {
                    cmd.stdout(out).stderr(err);
                }
                None => {
                    cmd.stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null());
                }
            }
        }

        let mut child = cmd.spawn().map_err(PaperedError::Io)?;
        println!(
            "{} {}",
            "Starting daemon:".green().bold(),
            daemon_bin.display()
        );
        if foreground {
            let status = child.wait().map_err(PaperedError::Io)?;
            let code = status.code().unwrap_or(1);
            if code != 0 {
                return Err(PaperedError::unknown(format!(
                    "Daemon exited with code {code}"
                )));
            }
        } else {
            // Wait until the daemon has registered (its RegistrationGuard
            // writes the PID file early in startup) before returning.
            // Returning right after spawn would misreport twice over:
            // `papered stop` run in the not-yet-registered window falsely
            // reports "Daemon is not running", and a daemon that dies during
            // startup (e.g. another instance already running) would be
            // reported as a successful start.
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            loop {
                if let Some(status) = child.try_wait().map_err(PaperedError::Io)? {
                    let code = status.code().unwrap_or(1);
                    return Err(PaperedError::unknown(format!(
                        "Daemon exited with code {code} during startup. \
                         Check the daemon log at {}",
                        daemon_log_file().display()
                    )));
                }
                if papered::util::process::running_daemon_pid() == Some(child.id()) {
                    println!("{} {}", "Daemon PID:".cyan(), child.id());
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    eprintln!(
                        "{}",
                        "Daemon has not registered within 30s — it may still be starting.".yellow()
                    );
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        return Ok(());
    }

    if let Commands::OptimizeImages { dry_run } = cli.command {
        return cli::handle_optimize_images(dry_run).await;
    }
    if let Commands::Metrics = cli.command {
        return cli::handle_metrics().await;
    }
    if let Commands::Stop = cli.command {
        return cli::handle_stop().await;
    }
    if let Commands::Reset { force, all } = cli.command {
        return cli::handle_reset(force, all).await;
    }

    let client = ensure_daemon_running().await?;

    match cli.command {
        Commands::Daemon { .. }
        | Commands::OptimizeImages { .. }
        | Commands::Metrics
        | Commands::Stop
        | Commands::Reset { .. } => unreachable!(),
        Commands::Ui => {
            let url = ui_url(&client.base_url);
            println!("Opening {url}");
            open_browser(&url);
            Ok(())
        }
        Commands::Add { path, r#type } => cli::handle_add(&client, path, r#type).await,
        Commands::Batch { path, recursive } => cli::handle_batch(&client, path, recursive).await,
        Commands::Search {
            query,
            section,
            limit,
            min_score,
            search_method,
        } => cli::handle_search(&client, query, section, limit, min_score, search_method).await,
        Commands::Similar {
            paper_id,
            section,
            limit,
        } => cli::handle_similar(&client, paper_id, section, limit).await,
        Commands::List { limit, offset } => cli::handle_list(&client, limit, offset).await,
        Commands::Show { paper_id } => cli::handle_show(&client, paper_id).await,
        Commands::Delete { paper_id } => cli::handle_delete(&client, paper_id).await,
        Commands::Reindex { paper_id, all } => cli::handle_reindex(&client, paper_id, all).await,
        Commands::Export {
            target,
            format,
            output,
            paper_id,
        } => cli::handle_export(&client, target, format, output, paper_id).await,
        Commands::Stats => cli::handle_stats(&client).await,
        Commands::Config { action } => cli::handle_config(&client, action).await,
        Commands::Model { action } => {
            let ModelAction::SwitchEmbedding {
                endpoint_id,
                reindex,
                dry_run,
            } = action;
            cli::model::run_switch_embedding(
                &client.client,
                &client.base_url,
                endpoint_id,
                reindex,
                dry_run,
            )
            .await
        }
        Commands::Lattice { action } => cli::handle_lattice(&client, action).await,
    }?;

    Ok(())
}

#[cfg(test)]
mod ui_command_tests {
    use super::*;

    #[test]
    fn browser_opener_targets_a_real_program() {
        let (program, args) = browser_opener("http://127.0.0.1:9321/ui/");
        assert!(!program.is_empty());
        assert!(args.iter().any(|a| a.contains("/ui/")));
    }

    #[test]
    fn ui_url_appends_ui_path() {
        assert_eq!(ui_url("http://127.0.0.1:9321"), "http://127.0.0.1:9321/ui/");
    }
}

#[cfg(test)]
mod daemon_log_rotation_tests {
    use super::*;
    use std::fs;

    #[test]
    fn rotates_log_that_exceeds_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("daemon.log");
        fs::write(&log, vec![b'x'; 100]).unwrap();

        assert!(rotate_daemon_log_if_oversized(&log, 50));
        assert!(!log.exists());
        assert_eq!(
            fs::read(dir.path().join("daemon.log.1")).unwrap().len(),
            100
        );
    }

    #[test]
    fn leaves_log_at_or_under_threshold_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("daemon.log");
        fs::write(&log, vec![b'x'; 50]).unwrap();

        assert!(!rotate_daemon_log_if_oversized(&log, 50));
        assert!(log.exists());
        assert!(!dir.path().join("daemon.log.1").exists());
    }

    #[test]
    fn missing_log_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("daemon.log");

        assert!(!rotate_daemon_log_if_oversized(&log, 50));
        assert!(!dir.path().join("daemon.log.1").exists());
    }

    #[test]
    fn rotation_replaces_an_existing_backup() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("daemon.log");
        fs::write(&log, vec![b'x'; 100]).unwrap();
        fs::write(dir.path().join("daemon.log.1"), b"stale").unwrap();

        assert!(rotate_daemon_log_if_oversized(&log, 50));
        assert!(!log.exists());
        assert_eq!(
            fs::read(dir.path().join("daemon.log.1")).unwrap().len(),
            100
        );
    }
}
