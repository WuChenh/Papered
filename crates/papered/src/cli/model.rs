use colored::Colorize;
use papered::error::Result;
use papered::paper::PaperStatus;
use papered::routes::*;
use serde::Deserialize;

#[derive(Deserialize)]
struct HealthResponse {
    status: String,
    paper_count: usize,
    vector_count: usize,
    embedding_model: Option<String>,
}

#[derive(Deserialize)]
struct ImportQueueItem {
    #[serde(rename = "paper_id")]
    _paper_id: String,
    status: PaperStatus,
}

pub async fn run_switch_embedding(
    client: &reqwest::Client,
    base_url: &str,
    endpoint_id: String,
    reindex: bool,
    dry_run: bool,
) -> Result<()> {
    let health_resp = client.get(format!("{base_url}{API_HEALTH}")).send().await?;
    if !health_resp.status().is_success() {
        return Err(papered::PaperedError::unknown(
            "Failed to fetch daemon health status".to_string(),
        ));
    }
    let health: HealthResponse = health_resp.json().await?;

    let config_resp = client.get(format!("{base_url}{API_CONFIG}")).send().await?;
    if !config_resp.status().is_success() {
        return Err(papered::PaperedError::unknown(
            "Failed to fetch daemon config".to_string(),
        ));
    }
    #[derive(Deserialize)]
    struct PurposesCfg {
        embedding: String,
    }
    #[derive(Deserialize)]
    struct Cfg {
        purposes: PurposesCfg,
    }
    let config: Cfg = config_resp.json().await?;

    println!("{}", "Current Embedding Configuration".bold().underline());
    println!("  Endpoint:  {}", config.purposes.embedding.cyan());
    println!(
        "  Model:     {}",
        health.embedding_model.as_deref().unwrap_or("N/A").cyan()
    );
    println!();

    let new_endpoint = endpoint_id.clone();

    println!("{}", "New Embedding Configuration".bold().underline());
    println!("  Endpoint:  {}", new_endpoint.cyan());
    println!();

    let endpoint_changed = new_endpoint != config.purposes.embedding;

    if endpoint_changed {
        println!("{}", "Changes detected:".yellow().bold());
        println!(
            "  • Endpoint: {} → {}",
            config.purposes.embedding.yellow(),
            new_endpoint.green()
        );
        if reindex {
            println!(
                "  • {} papers will be reindexed",
                health.paper_count.to_string().red().bold()
            );
        } else {
            println!(
                "  • {}",
                "Reindex NOT enabled — existing vectors will become stale."
                    .red()
                    .bold()
            );
        }
        println!();
    } else {
        println!("{}", "No changes detected. Nothing to do.".yellow());
        return Ok(());
    }

    if dry_run {
        println!("{}", "Dry-run mode — no changes made.".green().bold());
        println!("To apply changes, run without --dry-run");
        return Ok(());
    }

    print!(
        "{} ",
        "Proceed with embedding model switch? [y/N] "
            .yellow()
            .bold()
    );
    std::io::Write::flush(&mut std::io::stdout())?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    if !input.trim().eq_ignore_ascii_case("y") {
        println!("{}", "Cancelled.".red());
        return Ok(());
    }

    let resp = client
        .post(format!("{base_url}{API_CONFIG_EMBEDDING}"))
        .json(&serde_json::json!({
            "model_key": if endpoint_id.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(endpoint_id) },
            "reembed_all": reindex,
        }))
        .send()
        .await?;

    if !resp.status().is_success() {
        let text = resp.text().await?;
        return Err(papered::PaperedError::unknown(format!("Error: {text}")));
    }

    println!("{}", "Embedding model updated successfully".green().bold());

    if reindex {
        println!("{}", "Reindexing all papers...".yellow());
        for _ in 0..30 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            let queue_resp = client
                .get(format!("{base_url}{API_IMPORT_QUEUE}"))
                .send()
                .await?;
            let queue: Vec<ImportQueueItem> = queue_resp.json().await?;
            let processing = queue
                .iter()
                .filter(|i| i.status == PaperStatus::Processing)
                .count();

            if processing == 0 {
                println!("{}", "Reindex complete!".green().bold());
                break;
            }

            print!(
                "\r  {} papers still processing... ",
                processing.to_string().yellow()
            );
            std::io::Write::flush(&mut std::io::stdout())?;
        }

        let final_health = client.get(format!("{base_url}{API_HEALTH}")).send().await?;
        if let Ok(fh) = final_health.json::<HealthResponse>().await {
            println!("\n  Status:  {}", fh.status.green());
            println!("  Papers:  {}", fh.paper_count.to_string().cyan());
            println!("  Vectors: {}", fh.vector_count.to_string().cyan());
            println!(
                "  Model:   {}",
                fh.embedding_model.as_deref().unwrap_or("N/A").cyan()
            );
        }
    }

    Ok(())
}
