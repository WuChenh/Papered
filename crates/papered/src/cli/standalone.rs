use papered::AppConfig;
use papered::error::Result;

pub fn stop_daemon() {
    if cfg!(windows) {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/IM", "papered-daemon.exe"])
            .status();
    } else {
        let _ = std::process::Command::new("pkill")
            .args(["-f", "papered-daemon"])
            .status();
    }
}

pub async fn handle_stop() -> Result<()> {
    use colored::Colorize;

    let port_file = papered::routes::daemon_port_file();
    if !port_file.exists() {
        println!("{}", "Daemon is not running.".yellow());
        return Ok(());
    }

    println!("{}", "Stopping daemon...".bold());
    stop_daemon();

    for _ in 0..20 {
        if !port_file.exists() {
            println!("{}", "Daemon stopped.".green().bold());
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    println!(
        "{} {}",
        "Daemon stopped.".green().bold(),
        "(port file may remain — ignore or remove manually)".dimmed()
    );
    Ok(())
}

pub async fn handle_reset(force: bool, all: bool) -> Result<()> {
    use colored::Colorize;

    let config = AppConfig::load()?;
    let data_dir = &config.data_dir;

    stop_daemon();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let mut paths_to_remove: Vec<std::path::PathBuf> = vec![
        data_dir.join("papered.db"),
        data_dir.join("papered.db-wal"),
        data_dir.join("papered.db-shm"),
        data_dir.join("papers"),
        data_dir.join("covers"),
    ];
    if all {
        paths_to_remove.push(data_dir.join("logs"));
        paths_to_remove.push(data_dir.join("cache"));
    }

    let mut total_bytes = 0u64;
    for path in &paths_to_remove {
        if path.exists() {
            total_bytes += if path.is_dir() {
                papered::util::fs::dir_size(path).unwrap_or(0)
            } else {
                std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
            };
        }
    }

    println!("{}", "Papered reset".bold());
    println!(
        "  Data directory: {}",
        data_dir.display().to_string().cyan()
    );
    println!(
        "  Total to remove: {}",
        papered::util::fs::human_readable_size(total_bytes).cyan()
    );
    println!("  Files/directories:");
    for path in &paths_to_remove {
        if path.exists() {
            println!("    {}", path.display().to_string().red());
        } else {
            println!("    {} (not found)", path.display().to_string().dimmed());
        }
    }

    if !force {
        println!();
        println!(
            "{} Run with --force to actually delete these files.",
            "Preview only.".yellow()
        );
        return Ok(());
    }

    for path in &paths_to_remove {
        if !path.exists() {
            continue;
        }
        let result = if path.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
        if let Err(e) = result {
            eprintln!(
                "{} Failed to remove {}: {e}",
                "Warning:".yellow(),
                path.display()
            );
        }
    }

    for sub in &["papers", "covers", "logs", "cache"] {
        let _ = std::fs::create_dir_all(data_dir.join(sub));
    }

    println!();
    println!(
        "{}",
        "Reset complete. Config file preserved.".green().bold()
    );
    Ok(())
}

pub async fn handle_optimize_images(dry_run: bool) -> Result<()> {
    use colored::Colorize;

    let config = AppConfig::load()?;
    let db_path = config.db_path();
    let store = papered::create_store(&db_path).await?;

    println!(
        "{} {} (dry_run={})",
        "Optimizing images in".bold(),
        config.data_dir.display().to_string().cyan(),
        dry_run
    );
    println!(
        "  format={} max_long_side={} quality={}",
        config.pdf_extraction.output_format.cyan(),
        config
            .pdf_extraction
            .output_max_long_side
            .to_string()
            .cyan(),
        config.pdf_extraction.output_quality.to_string().cyan()
    );

    let stats = papered::util::image::optimize_existing_images(
        &config.data_dir,
        store,
        &config.pdf_extraction,
        dry_run,
    )
    .await?;

    println!();
    println!("{}", "Optimization complete".green().bold());
    println!(
        "  Papers scanned: {}",
        stats.papers_scanned.to_string().cyan()
    );
    println!(
        "  Images processed: {} (skipped: {}, failed: {})",
        stats.images_processed.to_string().cyan(),
        stats.images_skipped.to_string().yellow(),
        stats.images_failed.to_string().red()
    );
    println!(
        "  Size: {} → {} ({:.1}% of original)",
        papered::util::fs::human_readable_size(stats.bytes_before).cyan(),
        papered::util::fs::human_readable_size(stats.bytes_after).cyan(),
        if stats.bytes_before > 0 {
            (stats.bytes_after as f64 / stats.bytes_before as f64) * 100.0
        } else {
            0.0
        }
    );
    if dry_run {
        println!(
            "\n{} Run without --dry-run to apply changes.",
            "Preview only.".yellow()
        );
    }
    Ok(())
}

pub async fn handle_metrics() -> Result<()> {
    use colored::Colorize;

    let config = AppConfig::load()?;
    let db_path = config.db_path();
    let store = papered::create_store(&db_path).await?;

    let (all_time, last_24h) = papered::llm::metrics::fetch_both_metrics(&*store).await?;

    println!("{}", "LLM Call Metrics".bold().underline());
    println!("  DB: {}", db_path.display());
    if all_time.is_empty() && last_24h.is_empty() {
        println!();
        println!(
            "{}",
            "No LLM call metrics recorded yet — they appear after the daemon handles searches or Q&A."
                .yellow()
        );
        return Ok(());
    }
    println!();
    print_metrics_table("All time", &all_time);
    println!();
    print_metrics_table("Last 24h", &last_24h);
    Ok(())
}

pub fn print_metrics_table(title: &str, groups: &[papered::llm::metrics::LlmCallMetricGroup]) {
    use colored::Colorize;

    println!("{}", title.bold());
    if groups.is_empty() {
        println!("  {}", "(no calls in this window)".dimmed());
        return;
    }
    println!(
        "  {:<10} {:<40} {:>6} {:>8} {:>14} {:>17} {:>15}",
        "kind",
        "model",
        "calls",
        "failures",
        "prompt_tokens",
        "completion_tokens",
        "avg_latency_ms",
    );
    for g in groups {
        println!(
            "  {:<10} {:<40} {:>6} {:>8} {:>14} {:>17} {:>15.0}",
            g.kind,
            g.model,
            g.calls,
            g.failures,
            g.prompt_tokens,
            g.completion_tokens,
            g.avg_latency_ms,
        );
    }
}
