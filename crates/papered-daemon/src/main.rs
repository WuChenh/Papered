//! Papered background daemon — HTTP API, MCP server.

use axum::routing::get;
use papered::{AppConfig, error::Result};
use papered_mcp::{build_mcp_service, run_stdio_server};
use std::io;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::RwLock;
use tokio::task::JoinSet;
use tower_http::cors::CorsLayer;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, reload, util::SubscriberInitExt};

mod api;
mod state;
mod sync_runner;
mod ui;
mod worker;
pub(crate) use state::AppState;

fn write_port_file(port: u16) -> io::Result<()> {
    let port_dir = papered::routes::daemon_port_dir();
    std::fs::create_dir_all(&port_dir)?;
    let tmp_path = papered::routes::daemon_port_tmp_file();
    let final_path = papered::routes::daemon_port_file();
    {
        let mut f = std::fs::File::create(&tmp_path)?;
        use std::io::Write;
        writeln!(f, "{port}")?;
        f.flush()?;
    }
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(())
}

/// Write the daemon PID file (best-effort) so CLI clients can tell a
/// live-but-still-starting daemon apart from a stale registration left by an
/// ungraceful exit.
fn write_pid_file() {
    let path = papered::routes::daemon_pid_file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&path, format!("{}\n", std::process::id())) {
        tracing::warn!("Failed to write daemon PID file: {e}");
    }
}

/// Removes the daemon's registration files on exit — graceful shutdown and
/// early error returns alike. The PID file is removed only while it still
/// names this process, and the port file only if this process wrote it, so a
/// losing daemon in a startup race cannot unregister the winner.
struct RegistrationGuard {
    owns_port_file: bool,
}

impl RegistrationGuard {
    /// Fail fast with a clear message when another daemon is already running,
    /// then register this process. The database lock remains the backstop for
    /// startup races this check cannot see, but a clear message beats a turso
    /// locking error.
    fn acquire() -> Self {
        if let Some(pid) = papered::util::process::running_daemon_pid() {
            eprintln!(
                "Another papered daemon is already running (pid {pid}). \
                 Stop it with `papered stop` or wait for it to exit."
            );
            std::process::exit(1);
        }
        write_pid_file();
        Self {
            owns_port_file: false,
        }
    }
}

impl Drop for RegistrationGuard {
    fn drop(&mut self) {
        let pid_file = papered::routes::daemon_pid_file();
        let still_ours = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|c| c.trim().parse::<u32>().ok())
            == Some(std::process::id());
        if still_ours {
            let _ = std::fs::remove_file(&pid_file);
        }
        if self.owns_port_file {
            let _ = std::fs::remove_file(papered::routes::daemon_port_file());
        }
    }
}

/// Wait for SIGTERM or SIGINT and log which one arrived.
#[cfg(unix)]
async fn wait_for_shutdown_signal() -> io::Result<()> {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    tokio::select! {
        _ = term.recv() => tracing::info!("Received SIGTERM, shutting down gracefully"),
        _ = interrupt.recv() => tracing::info!("Received SIGINT, shutting down gracefully"),
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let use_stdio = std::env::args().any(|a| a == "--stdio");

    // --- Config / logging ---
    let config = AppConfig::load()?;
    std::fs::create_dir_all(&config.data_dir)?;

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.log_level));
    let (reload_layer, reload_handle) = reload::Layer::new(filter);
    tracing_subscriber::registry()
        .with(reload_layer)
        .with(tracing_subscriber::fmt::layer())
        .init();
    let _ = state::LOG_RELOAD_HANDLE.set(reload_handle);

    tracing::info!("Papered daemon starting...");

    // --- Single-instance registration ---
    // --stdio instances are spawned per MCP client and only speak MCP over
    // stdio; they neither bind the HTTP port nor take part in HTTP-daemon
    // registration.
    let mut registration = (!use_stdio).then(RegistrationGuard::acquire);

    // --- Store (includes stale-paper recovery) ---
    let store = state::init_store(&config).await?;

    // --- Clients ---
    // Tolerate unconfigured purposes (fresh install before the setup wizard):
    // fall back to placeholder clients and let the recovery watcher / config
    // update path swap in the real clients once the config is completed.
    let embedding = state::build_embedding_or_placeholder(
        &config,
        papered::llm::metrics::store_metrics_sink(&store),
        "starting in degraded mode",
    );
    let reranker = state::build_reranker_or_placeholder(
        &config,
        papered::llm::metrics::store_metrics_sink(&store),
        "starting in degraded mode",
    );
    // --- Shared arc wrappers ---
    let config_arc = Arc::new(RwLock::new(config.clone()));

    let embedding_arc = Arc::new(RwLock::new(embedding.clone()));
    let reranker_arc = Arc::new(RwLock::new(reranker.clone()));

    // --- Engines ---
    let (search_engine, rag_engine, indexer) =
        state::build_engines(store.clone(), embedding.clone(), reranker.clone(), &config)
            .await
            .map_err(|e| {
                papered::PaperedError::Config(format!("Failed to create engines: {e}"), None)
            })?;
    let search_engine = Arc::new(RwLock::new(search_engine));
    let rag_engine = Arc::new(RwLock::new(rag_engine));
    let indexer = Arc::new(RwLock::new(indexer));

    // --- Channels ---
    let indexing_cfg = config.indexing.clone();
    let (import_tx, import_rx) =
        tokio::sync::mpsc::channel::<papered::util::IndexJob>(indexing_cfg.queue_size);

    // --- Zotero sync channel (serializes manual and automatic requests) ---
    let (zotero_sync_tx, zotero_sync_rx) =
        tokio::sync::mpsc::channel::<crate::state::ZoteroSyncRequest>(16);

    // --- AppState ---

    let indexing_paused_at_boot = crate::state::index_pause_flag_path(&config).exists();
    if indexing_paused_at_boot {
        tracing::info!("Indexing pause flag found — worker pool starts paused");
    }
    let state = Arc::new(AppState::new(
        store.clone(),
        embedding_arc.clone(),
        reranker_arc.clone(),
        config_arc.clone(),
        search_engine,
        rag_engine,
        indexer,
        import_tx,
        zotero_sync_tx,
        indexing_paused_at_boot,
    ));

    // NOTE: the embedding model is deliberately NOT probed here. A probe is a
    // network call whose worst case is unbounded (an endpoint that drops
    // packets burns the full client timeout), and everything before the HTTP
    // bind must stay local and bounded so the daemon becomes reachable — and
    // writes its port file — within a short, predictable window. The recovery
    // watcher below performs the initial probe (its first tick is immediate)
    // and keeps retrying until the model is reachable.

    // --- Sync workers ---
    state.spawn_lattice_sync().await;
    AppState::start_zotero_sync_worker(state.clone(), zotero_sync_rx).await;
    state.spawn_zotero_sync().await;

    // --- Background tasks ---
    let mut background_tasks = JoinSet::new();

    worker::spawn_indexing_worker_pool(
        state.clone(),
        import_rx,
        state.import_tx.clone(),
        indexing_cfg.concurrency,
        &mut background_tasks,
    );

    // Recovery watcher is always on: it self-terminates once the model is ready, and
    // keeps retrying whenever readiness flips to false at runtime — not only when the
    // model was already down at startup.
    let retry_state = state.clone();
    background_tasks.spawn(async move {
        tracing::info!("Starting embedding model recovery watcher (30s interval)");
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            // The first interval tick completes immediately, so the initial
            // probe runs at startup and then every 30s until the model is ready.
            interval.tick().await;
            if retry_state.embedding_model_ready.load(Ordering::Relaxed) {
                break;
            }
            let config = retry_state.config.read().await;
            let Ok(embedding) = state::build_embedding_client(&config) else {
                continue;
            };
            *retry_state.embedding.write().await = embedding.with_metrics(
                papered::llm::metrics::store_metrics_sink(&retry_state.store),
            );
            drop(config);
            // A fingerprint mismatch means the stored vectors came from a
            // different model than the configured one: force a rebuild even
            // when the dimension is unchanged. Otherwise an outage alone must
            // not destroy existing vectors — rebuild only when the dimension
            // actually differs.
            let policy = retry_state.probe_rebuild_policy().await;
            match retry_state.handle_embedding_model_change(policy).await {
                Ok(change) => {
                    if change.rebuilt {
                        let total = retry_state.reembed_all_now().await;
                        tracing::info!(
                            "Embedding model changed — queued {total} papers for re-embed"
                        );
                    } else {
                        tracing::info!(
                            "Embedding model ready (dimension {} unchanged); vectors preserved",
                            change.detected_dim
                        );
                    }
                    break;
                }
                Err(state::EmbeddingChangeError::Probe(e)) => {
                    tracing::debug!("Embedding model still unavailable: {e}");
                }
                Err(state::EmbeddingChangeError::Reset(e)) => {
                    tracing::warn!("Embedding store prepare after recovery failed: {e}");
                    break;
                }
            }
        }
    });

    state::start_config_watcher(state.clone(), &mut background_tasks);

    // --- MCP service ---
    let mcp_service = build_mcp_service(
        store.clone(),
        state.search_engine.clone(),
        state.rag_engine.clone(),
    );

    // --- HTTP router ---
    let cors = CorsLayer::new()
        .allow_origin([
            "http://localhost".parse().expect("valid URL"),
            "http://127.0.0.1".parse().expect("valid URL"),
            "https://localhost".parse().expect("valid URL"),
            "https://127.0.0.1".parse().expect("valid URL"),
        ])
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::DELETE,
            axum::http::Method::PUT,
            axum::http::Method::PATCH,
        ])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
        ]);

    let app = api::api_router()
        .route("/", get(ui::ui_redirect))
        .route("/ui", get(ui::ui_redirect))
        .route("/ui/", get(ui::serve_index))
        .route("/ui/{*path}", get(ui::serve_ui))
        .nest_service("/mcp/v1/messages", mcp_service)
        .layer(cors)
        .layer(tower_http::limit::RequestBodyLimitLayer::new(
            200 * 1024 * 1024,
        ))
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            std::time::Duration::from_secs(120),
        ))
        .with_state(state.clone());

    // --- MCP stdio path ---
    if use_stdio {
        tracing::info!("Starting Papered MCP stdio transport");
        run_stdio_server(
            store.clone(),
            state.search_engine.clone(),
            state.rag_engine.clone(),
        )
        .await;
        background_tasks.shutdown().await;
        return Ok(());
    }

    // --- Bind and listen ---
    // Binding happens only after initialization so the port file — written
    // right after a successful bind — always means "this process is serving
    // HTTP on this port". --stdio instances never reach this point and thus
    // cannot clobber a running HTTP daemon's registration.
    const BASE_PORT: u16 = papered::routes::DAEMON_DEFAULT_PORT;
    const MAX_PORT_TRIES: u16 = papered::routes::DAEMON_MAX_PORT_TRIES;
    let mut listener = None;
    for offset in 0..MAX_PORT_TRIES {
        let port = BASE_PORT + offset;
        match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            Ok(l) => {
                tracing::info!("Papered daemon listening on http://{}", l.local_addr()?);
                listener = Some(l);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                tracing::warn!("Port {} is in use, trying next port", port);
            }
            Err(e) => return Err(papered::PaperedError::Io(e)),
        }
    }
    let listener = listener.ok_or_else(|| {
        papered::PaperedError::Io(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!(
                "All ports {}-{} are in use",
                BASE_PORT,
                BASE_PORT + MAX_PORT_TRIES - 1
            ),
        ))
    })?;
    let addr = listener.local_addr().map_err(papered::PaperedError::Io)?;

    // --- Port file ---
    write_port_file(addr.port()).unwrap_or_else(|e| {
        tracing::warn!("Failed to write daemon port file: {}", e);
    });
    if let Some(reg) = &mut registration {
        reg.owns_port_file = true;
    }

    // --- Signal handling and graceful shutdown ---
    // Registration files are removed by the RegistrationGuard on return.
    //
    // Axum's graceful shutdown waits for *all* in-flight connections to
    // complete before `serve` returns. Long-lived MCP streamable-HTTP (SSE)
    // sessions and slow indexing requests (up to the 120s request timeout)
    // can hold that open indefinitely, which makes `papered stop` appear to
    // hang. Race the drain against a grace timer armed when the signal
    // arrives; past the grace window we force-exit (aborting workers) so
    // `papered stop` always terminates the daemon within a bounded time.
    const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(10);
    let (signal_tx, signal_rx) = tokio::sync::oneshot::channel::<()>();
    let shutdown = async move {
        #[cfg(unix)]
        {
            if let Err(e) = wait_for_shutdown_signal().await {
                tracing::error!("Signal handler setup failed ({e}) — shutting down");
            }
        }
        #[cfg(windows)]
        {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("Received Ctrl+C, shutting down gracefully");
        }
        let _ = signal_tx.send(());
    };

    // --- Serve ---
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .into_future();
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => {
            result.map_err(|e| papered::PaperedError::io_other(e.to_string()))?;
        }
        // The timer starts only once the shutdown signal has been received.
        _ = async {
            let _ = signal_rx.await;
            tokio::time::sleep(SHUTDOWN_GRACE).await;
        } => {
            tracing::warn!(
                "Graceful shutdown exceeded {}s — aborting background tasks and forcing exit",
                SHUTDOWN_GRACE.as_secs()
            );
            background_tasks.abort_all();
            if let Some(handle) = state.zotero_sync_worker_handle.lock().await.take() {
                handle.abort();
            }
            return Err(papered::PaperedError::io_other(
                "daemon shutdown timed out; forced exit",
            ));
        }
    }

    if let Some(handle) = state.zotero_sync_worker_handle.lock().await.take() {
        handle.abort();
    }
    background_tasks.shutdown().await;

    Ok(())
}
