//! Config file watching and log-level hot-reload.

use super::AppState;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use papered::AppConfig;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;
use tracing_subscriber::{EnvFilter, Registry, reload};

pub(crate) static LOG_RELOAD_HANDLE: std::sync::OnceLock<reload::Handle<EnvFilter, Registry>> =
    std::sync::OnceLock::new();

pub(crate) fn reload_log_level(level: &str) {
    if let Some(handle) = LOG_RELOAD_HANDLE.get() {
        let filter = tracing_subscriber::EnvFilter::new(level);
        if let Err(e) = handle.reload(filter) {
            tracing::warn!("Failed to reload log level: {}", e);
        } else {
            tracing::info!("Log level reloaded to: {}", level);
        }
    }
}

async fn handle_config_change(state: &Arc<AppState>) -> papered::error::Result<()> {
    let new_config = AppConfig::load()?;

    let current = state.config.read().await.clone();
    if new_config == current {
        return Ok(());
    }

    new_config.validate_strict()?;

    let _guard = state.config_write_lock.lock().await;
    if let Err(e) = new_config.save() {
        tracing::warn!("Failed to sync config to primary path: {}", e);
    }

    state.apply_config_update(&new_config, &current).await;

    Ok(())
}

pub fn start_config_watcher(state: Arc<AppState>, tasks: &mut JoinSet<()>) {
    let config_path = match AppConfig::config_path() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Cannot determine config path for file watcher: {}", e);
            return;
        }
    };
    let config_dir = match config_path.parent() {
        Some(d) => d.to_path_buf(),
        None => {
            tracing::error!("Config path has no parent directory");
            return;
        }
    };

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();

    let mut watcher = match RecommendedWatcher::new(
        move |event: notify::Result<notify::Event>| {
            if let Ok(event) = event {
                // Ignore remove/rename events — editors often create temp files.
                if !event.kind.is_modify() && !event.kind.is_create() {
                    return;
                }
                if event.paths.iter().any(|p| p.ends_with("config.toml")) {
                    let _ = tx.send(());
                }
            }
        },
        notify::Config::default(),
    ) {
        Ok(w) => w,
        Err(e) => {
            tracing::error!("Failed to create file watcher: {}", e);
            return;
        }
    };

    if let Err(e) = watcher.watch(&config_dir, RecursiveMode::NonRecursive) {
        tracing::warn!(
            "Failed to watch config directory '{}': {} (config hot-reload disabled)",
            config_dir.display(),
            e
        );
        return;
    }

    tracing::info!("Watching config file: {}", config_path.display());

    tasks.spawn(async move {
        let _watcher = watcher;
        let mut last_content: Option<String> = None;
        loop {
            rx.recv().await;
            tokio::time::sleep(Duration::from_millis(500)).await;
            while rx.try_recv().is_ok() {}

            // Deduplicate by content to avoid reloading identical files
            // (e.g. timestamp-only changes or duplicate events).
            let content = match tokio::fs::read_to_string(&config_path).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("Config file unreadable during hot-reload check: {}", e);
                    continue;
                }
            };
            if last_content.as_ref() == Some(&content) {
                continue;
            }
            last_content = Some(content);

            if let Err(e) = handle_config_change(&state).await {
                tracing::warn!("Config reload failed: {}", e);
            }
        }
    });
}
