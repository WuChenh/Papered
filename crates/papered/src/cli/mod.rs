mod import_;
pub mod model;
mod papers;
mod search;
mod standalone;

use colored::Colorize;
use papered::client::DaemonClient;
use papered::error::Result;

pub use import_::{handle_add, handle_batch};
pub use papers::{handle_delete, handle_list, handle_show};
pub use search::{handle_search, handle_similar};
pub use standalone::{handle_metrics, handle_optimize_images, handle_reset, handle_stop};

#[derive(clap::Subcommand)]
pub enum ConfigAction {
    Show,
    Set { key: String, value: String },
    Edit,
}

#[derive(clap::Subcommand)]
pub enum ModelAction {
    SwitchEmbedding {
        endpoint_id: String,
        #[arg(long)]
        reindex: bool,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(clap::Subcommand)]
pub enum LatticeAction {
    Status,
    Collections,
    List {
        query: Option<String>,
        #[arg(short, long, default_value = "20")]
        limit: u32,
    },
    Import {
        paper_id: String,
        #[arg(short, long)]
        pdf: Option<String>,
    },
    Sync,
}

pub const SECTION_DISPLAY_MAX_CHARS: usize = 200;

pub async fn handle_reindex(
    client: &DaemonClient,
    paper_id: Option<String>,
    all: bool,
) -> Result<()> {
    use papered::client::api_get_json;
    use papered::client::api_post_json;
    use papered::routes::API_PAPERS;

    if all {
        let mut offset = 0;
        let mut queued = 0;
        let total = loop {
            let resp: papered::paper::ListPapersResponse =
                api_get_json(client, &format!("{API_PAPERS}?limit=1000&offset={offset}")).await?;
            if resp.papers.is_empty() {
                break resp.total;
            }
            for paper in &resp.papers {
                api_post_json::<papered::paper::Paper>(
                    client,
                    &format!("{API_PAPERS}/{}/reindex", paper.id),
                    None,
                )
                .await?;
                queued += 1;
            }
            if !resp.has_more {
                break resp.total;
            }
            offset += resp.papers.len();
        };
        println!(
            "{}",
            format!("✓ Queued {queued} paper(s) for re-indexing (total {total})")
                .green()
                .bold()
        );
        return Ok(());
    }

    let paper_id = paper_id.ok_or_else(|| {
        papered::PaperedError::invalid_argument(
            "Provide a PAPER_ID or use --all to reindex the entire library",
        )
    })?;
    let paper: papered::paper::Paper =
        api_post_json(client, &format!("{API_PAPERS}/{paper_id}/reindex"), None).await?;
    println!(
        "{}",
        format!("✓ Paper {paper_id} re-indexed: {}", paper.title)
            .green()
            .bold()
    );
    Ok(())
}

pub async fn handle_export(
    client: &DaemonClient,
    target: String,
    format: String,
    output: String,
    paper_id: Option<String>,
) -> Result<()> {
    use colored::Colorize;
    use papered::client::api_post_json;
    use papered::routes::API_EXPORT;

    let paper_ids = paper_id.map(|id| vec![id]);
    let result: papered::index::export::ExportResult = api_post_json(
        client, API_EXPORT,
        Some(serde_json::json!({ "target": target, "format": format, "destination": output, "paper_ids": paper_ids })),
    ).await?;
    println!(
        "{}",
        format!(
            "✓ Exported {} paper(s) to {} file(s)",
            result.papers_exported,
            result.files_created.len()
        )
        .green()
        .bold()
    );
    for file in &result.files_created {
        println!("  {}", file.cyan());
    }
    Ok(())
}

pub async fn handle_stats(client: &DaemonClient) -> Result<()> {
    use colored::Colorize;
    use papered::client::api_get_json;
    use papered::routes::API_STATS;
    use papered::util::fs::human_readable_size;

    #[derive(serde::Deserialize)]
    struct StorageBreakdown {
        db_bytes: u64,
        images_bytes: u64,
        covers_bytes: u64,
    }

    #[derive(serde::Deserialize)]
    struct StatsResponse {
        papers: usize,
        vectors: usize,
        figures: usize,
        data_dir: String,
        storage: StorageBreakdown,
    }

    let stats: StatsResponse = api_get_json(client, API_STATS).await?;
    println!("{}", "Knowledge Base Statistics".bold().underline());
    println!("  Papers:  {}", stats.papers.to_string().cyan().bold());
    println!("  Vectors: {}", stats.vectors.to_string().cyan().bold());
    println!("  Figures: {}", stats.figures.to_string().cyan().bold());
    println!("  Data Dir: {}", stats.data_dir);
    println!(
        "  Storage: {} db, {} images, {} covers",
        human_readable_size(stats.storage.db_bytes),
        human_readable_size(stats.storage.images_bytes),
        human_readable_size(stats.storage.covers_bytes),
    );
    Ok(())
}

pub async fn handle_config(client: &DaemonClient, action: ConfigAction) -> Result<()> {
    use colored::Colorize;
    use papered::AppConfig;
    use papered::client::{api_get_json, api_post_json};
    use papered::routes::API_CONFIG;

    match action {
        ConfigAction::Show => {
            let config: AppConfig = api_get_json(client, API_CONFIG).await?;
            let config_str = toml::to_string_pretty(&config).map_err(|e| {
                papered::PaperedError::config(format!("Failed to serialize config: {e}"))
            })?;
            println!("{config_str}");
        }
        ConfigAction::Set { key, value } => {
            let mut config: AppConfig = api_get_json(client, API_CONFIG).await?;
            let value_str = value.clone();

            let key = match key.as_str() {
                "mineru.endpoint" => {
                    println!(
                        "{}",
                        "mineru.endpoint is removed — endpoint URLs are hardcoded per mode"
                            .yellow()
                    );
                    return Ok(());
                }
                "embedding.dimensions" => {
                    eprintln!(
                        "{}",
                        "embedding.dimensions is no longer configurable — dimension is auto-detected from the embedding API"
                            .yellow()
                    );
                    return Ok(());
                }
                "mineru.timeout" => "mineru.timeout_secs".to_string(),
                _ => key,
            };

            match key.as_str() {
                "embedding.model" => {
                    let model = config
                        .models
                        .get_mut(&config.purposes.embedding)
                        .ok_or_else(|| {
                            papered::PaperedError::config(format!(
                                "Embedding model '{}' not found",
                                config.purposes.embedding
                            ))
                        })?;
                    model.model = value;
                }
                "embedding.api_base" => {
                    let model = config
                        .models
                        .get(&config.purposes.embedding)
                        .ok_or_else(|| {
                            papered::PaperedError::config(format!(
                                "Embedding model '{}' not found",
                                config.purposes.embedding
                            ))
                        })?;
                    let provider_key = model.provider.clone();
                    config
                        .providers
                        .get_mut(&provider_key)
                        .ok_or_else(|| {
                            papered::PaperedError::config(format!(
                                "Provider '{provider_key}' not found"
                            ))
                        })?
                        .api_base = value;
                }
                "section.extraction_model" => {
                    let model =
                        config
                            .models
                            .get_mut(&config.purposes.section)
                            .ok_or_else(|| {
                                papered::PaperedError::config(format!(
                                    "Section model '{}' not found",
                                    config.purposes.section
                                ))
                            })?;
                    model.model = value;
                }
                "mineru.enabled" => {
                    config.mineru.enabled = value.parse().map_err(|e| {
                        papered::PaperedError::invalid_argument(format!(
                            "Invalid boolean '{value}': {e}"
                        ))
                    })?;
                    println!(
                        "{}",
                        format!("MinerU enabled: {}", config.mineru.enabled).cyan()
                    );
                }
                "mineru.mode" => {
                    config.mineru.mode = value.parse().map_err(|e| {
                        papered::PaperedError::invalid_argument(format!(
                            "Invalid mode '{value}': {e}"
                        ))
                    })?;
                }
                "mineru.timeout_secs" => {
                    let secs: u64 = value.parse().map_err(|e| {
                        papered::PaperedError::invalid_argument(format!(
                            "Invalid timeout '{value}': {e}"
                        ))
                    })?;
                    if secs == 0 {
                        return Err(papered::PaperedError::invalid_argument(
                            "timeout must be > 0",
                        ));
                    }
                    config.mineru.timeout_secs = secs;
                }
                "mineru.api_key" => config.mineru.api_key = Some(value),
                "lattice_sync.enabled" => {
                    config.lattice_sync.base.enabled = value.parse().map_err(|e| {
                        papered::PaperedError::invalid_argument(format!(
                            "Invalid boolean '{value}': {e}"
                        ))
                    })?;
                }
                "lattice_sync.interval_secs" => {
                    let secs: u64 = value.parse().map_err(|e| {
                        papered::PaperedError::invalid_argument(format!(
                            "Invalid number '{value}': {e}"
                        ))
                    })?;
                    config.lattice_sync.base.interval_secs = secs.max(60);
                }
                "lattice_sync.batch_limit" => {
                    let limit: u32 = value.parse().map_err(|e| {
                        papered::PaperedError::invalid_argument(format!(
                            "Invalid number '{value}': {e}"
                        ))
                    })?;
                    config.lattice_sync.base.batch_limit = limit.min(200);
                }
                _ => {
                    println!("{}", format!("Unknown config key: {key}").red());
                    return Ok(());
                }
            }
            let _update: serde_json::Value =
                api_post_json(client, API_CONFIG, Some(serde_json::to_value(&config)?)).await?;
            println!(
                "{}",
                format!("✓ Config updated: {key} = {value_str}").green()
            );
        }
        ConfigAction::Edit => {
            let path = papered::AppConfig::find_config_path().map_err(|e| {
                papered::PaperedError::io_other(format!("Failed to get config path: {e}"))
            })?;
            #[cfg(target_os = "macos")]
            {
                std::process::Command::new("open")
                    .arg("-t")
                    .arg(&path)
                    .spawn()
                    .ok();
            }
            #[cfg(not(target_os = "macos"))]
            {
                if let Ok(editor) = std::env::var("EDITOR") {
                    std::process::Command::new(&editor).arg(&path).spawn().ok();
                } else {
                    println!("Set $EDITOR to open config in your preferred editor");
                }
            }
            println!("Opening {} in editor", path.display());
        }
    }
    Ok(())
}

pub async fn handle_lattice(client: &DaemonClient, action: LatticeAction) -> Result<()> {
    use colored::Colorize;
    use papered::client::{api_get_json, api_request, check_response};
    use papered::routes::{
        API_IMPORT_LATTICE, API_LATTICE_COLLECTIONS, API_LATTICE_SEARCH, API_LATTICE_STATUS,
        API_LATTICE_SYNC,
    };

    match action {
        LatticeAction::Status => {
            #[derive(serde::Deserialize)]
            struct LsResp {
                available: bool,
                api_version: Option<String>,
                app_version: Option<String>,
                capabilities: Option<Vec<String>>,
                base_url: Option<String>,
            }
            let status: LsResp = api_get_json(client, API_LATTICE_STATUS).await?;
            println!("{}", "Lattice Status".bold().underline());
            println!(
                "  Available:  {}",
                if status.available {
                    "Yes".green()
                } else {
                    "No".red()
                }
            );
            if let Some(url) = &status.base_url {
                println!("  URL:        {}", url.cyan());
            }
            if let Some(api) = &status.api_version {
                println!("  API:        {api}");
            }
            if let Some(app) = &status.app_version {
                println!("  App:        {app}");
            }
            if let Some(caps) = &status.capabilities {
                println!("  Capabilities: {}", caps.join(", "));
            }
        }
        LatticeAction::Collections => {
            #[derive(serde::Deserialize)]
            struct CollItem {
                id: String,
                name: String,
                path: String,
            }
            #[derive(serde::Deserialize)]
            struct CollResp {
                collections: Vec<CollItem>,
            }
            let resp: CollResp = api_get_json(client, API_LATTICE_COLLECTIONS).await?;
            if resp.collections.is_empty() {
                println!("{}", "No collections found in Lattice.".yellow());
            } else {
                println!(
                    "{} {} collection(s)\n",
                    "✓".green().bold(),
                    resp.collections.len()
                );
                for c in &resp.collections {
                    println!("  {} {}", c.name.bold(), format!("({})", c.id).cyan());
                    println!("    path: {}", c.path.dimmed());
                }
            }
        }
        LatticeAction::List { query, limit } => {
            let q = query.unwrap_or_default();
            let resp = api_request(
                client,
                reqwest::Method::GET,
                &format!(
                    "{API_LATTICE_SEARCH}?q={}&limit={limit}",
                    papered::lattice::urlencoding(&q)
                ),
                None,
            )
            .await?;
            if resp.status() == reqwest::StatusCode::BAD_GATEWAY {
                println!(
                    "{}",
                    "Lattice is not running. Start Lattice first.".red().bold()
                );
                println!("{}", "papered lattice status  — check connection".dimmed());
                return Ok(());
            }
            let resp = check_response(resp).await?;
            let results: papered::lattice::LatticeSearchResponse = resp.json().await?;
            if results.papers.is_empty() {
                println!(
                    "{}",
                    if q.is_empty() {
                        "No recent papers in Lattice.".yellow()
                    } else {
                        format!("No results for: \"{q}\"").yellow()
                    }
                );
            } else {
                println!(
                    "{} {} paper(s) in Lattice\n",
                    "✓".green().bold(),
                    results.papers.len()
                );
                for paper in &results.papers {
                    println!("  {} {}", paper.id.cyan(), paper.title.bold());
                    println!(
                        "    {} ({})",
                        paper.authors_display.dimmed(),
                        paper.year_string()
                    );
                    println!("    citekey: {}", paper.citekey.dimmed());
                    println!();
                }
            }
        }
        LatticeAction::Import { paper_id, pdf } => {
            let resp = api_request(
                client,
                reqwest::Method::POST,
                API_IMPORT_LATTICE,
                Some(serde_json::json!({ "paper_id": paper_id, "file_path": pdf })),
            )
            .await?;
            let status = resp.status();
            if status == reqwest::StatusCode::BAD_GATEWAY {
                println!(
                    "{}",
                    "Lattice is not running. Start Lattice first.".red().bold()
                );
                println!("{}", "papered lattice status  — check connection".dimmed());
                return Ok(());
            }
            if status == reqwest::StatusCode::NOT_FOUND {
                println!(
                    "{}",
                    format!("Paper not found in Lattice: {paper_id}").red()
                );
                return Ok(());
            }
            let resp = check_response(resp).await?;
            let result: serde_json::Value = resp.json().await?;
            println!("{}", "✓ Imported from Lattice".green().bold());
            println!(
                "  Source: {}",
                result["source"].as_str().unwrap_or("").dimmed()
            );
            if result["indexing_queued"].as_bool().unwrap_or(false) {
                println!("  Indexing queued");
            } else {
                println!("  Metadata-only import (no PDF provided)");
            }
        }
        LatticeAction::Sync => {
            let resp = api_request(client, reqwest::Method::POST, API_LATTICE_SYNC, None).await?;
            if resp.status() == reqwest::StatusCode::BAD_GATEWAY {
                println!(
                    "{}",
                    "Lattice is not running. Start Lattice first.".red().bold()
                );
                return Ok(());
            }
            let resp = check_response(resp).await?;
            let report: papered::sync::SyncReport = resp.json().await?;
            println!("{}", "Lattice Sync Report".bold().underline());
            println!("  Imported:       {}", report.imported.to_string().green());
            println!("    with PDF:     {}", report.pdf_found.to_string().cyan());
            println!("    metadata-only: {}", report.metadata_only);
            println!("  Skipped:        {}", report.skipped);
            if !report.errors.is_empty() {
                println!(
                    "  Errors:         {}",
                    report.errors.len().to_string().red()
                );
                for err in &report.errors {
                    println!("    - {}", err.red());
                }
            }
        }
    }
    Ok(())
}
