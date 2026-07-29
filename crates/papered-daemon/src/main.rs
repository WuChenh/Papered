//! Papered background daemon — HTTP API, MCP server.

use axum::routing::get;
use papered::{AppConfig, error::Result};
use papered_mcp::{build_mcp_service, run_stdio_server};
use std::io;
use std::path::PathBuf;
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

    // --- Store (includes stale-paper recovery) ---
    let (store, placeholder_table, current_fingerprint) = state::init_store(&config).await?;

    let mut needs_model_change_reembed = false;

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
    ));

    // --- Embedding model test ---
    if placeholder_table {
        match state.test_and_prepare_embedding_store().await {
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    "Embedding model test failed: {}. Monitoring config for changes.",
                    e
                );
            }
        }
    } else {
        let stored_fingerprint = store.get_meta("embedding_fingerprint").await.ok().flatten();
        let models_changed = match &stored_fingerprint {
            Some(stored) => {
                let current = current_fingerprint.as_deref().unwrap_or("");
                *stored != current
            }
            None => false,
        };

        if models_changed {
            tracing::info!("Embedding model changed — testing new model");
            match state
                .handle_embedding_model_change(state::EmbeddingRebuildPolicy::ForceRebuild)
                .await
            {
                Ok(change) => {
                    if change.rebuilt {
                        needs_model_change_reembed = true;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "New embedding model test failed: {e}. Starting in degraded mode."
                    );
                }
            }
        } else {
            // Fingerprint unchanged, but still verify the model is actually reachable
            // instead of blindly trusting a past probe. `handle_embedding_model_change`
            // with `ProbeOnly` only probes (never touches vectors) and is the sole
            // writer of `embedding_model_ready`.
            tracing::info!("Embedding model unchanged — verifying reachability");
            if let Err(e) = state
                .handle_embedding_model_change(state::EmbeddingRebuildPolicy::ProbeOnly)
                .await
            {
                tracing::warn!(
                    "Embedding model unreachable at startup: {e}; recovery watcher will retry"
                );
            }
        }
    }

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
            // The first interval tick completes immediately, so the check runs
            // at startup and then every 30s.
            interval.tick().await;
            if retry_state.embedding_model_ready.load(Ordering::Relaxed) {
                break;
            }
            let config = retry_state.config.read().await;
            let Ok(embedding) = state::build_embedding_client(&config) else {
                continue;
            };
            *retry_state.embedding.write().await = embedding
                .with_metrics(papered::llm::metrics::store_metrics_sink(&retry_state.store));
            drop(config);
            // Recovery is not the same as a model change: if the model (and its
            // dimension) is unchanged, the existing vectors are still valid and
            // must be preserved. Only rebuild when the dimension actually differs.
            match retry_state
                .handle_embedding_model_change(state::EmbeddingRebuildPolicy::RebuildIfChanged)
                .await
            {
                Ok(change) => {
                    if change.rebuilt {
                        let total = retry_state.reembed_all_now().await;
                        tracing::info!(
                            "Embedding model changed during outage — queued {total} papers for re-embed"
                        );
                    } else {
                        tracing::info!(
                            "Embedding model recovered (dimension {} unchanged); vectors preserved",
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

    if needs_model_change_reembed {
        tracing::info!("Queuing all papers for re-embed after embedding model change");
        let state_for_reembed = state.clone();
        background_tasks.spawn(async move {
            state_for_reembed.reembed_all_now().await;
        });
    }

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

    // --- Bind and listen ---
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
    let config_dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    write_port_file(addr.port()).unwrap_or_else(|e| {
        tracing::warn!("Failed to write daemon port file: {}", e);
    });

    // --- Signal handling and graceful shutdown ---
    let port_file = config_dir.join("papered").join("daemon.port");
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
        if let Err(e) = std::fs::remove_file(&port_file) {
            tracing::debug!("Failed to remove port file: {}", e);
        }
    };

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

    // --- Serve ---
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(|e| papered::PaperedError::io_other(e.to_string()))?;

    if let Some(handle) = state.zotero_sync_worker_handle.lock().await.take() {
        handle.abort();
    }
    background_tasks.shutdown().await;

    Ok(())
}
