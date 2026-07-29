use papered::StrLabel;
use papered::client::DaemonClient;
use papered::error::Result;
use papered::paper::source::DocumentSource;
use papered::routes::*;

pub fn canonicalize_or_warn(path: &std::path::Path) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|e| {
        tracing::warn!(
            "canonicalize failed for {}: {}, using original path",
            path.display(),
            e
        );
        path.to_path_buf()
    })
}

pub async fn import_file(
    client: &DaemonClient,
    abs_path: &std::path::Path,
    source: DocumentSource,
) -> Result<reqwest::Response> {
    if source == DocumentSource::Pdf {
        return Ok(client
            .client
            .post(client.url(API_PAPERS))
            .json(&serde_json::json!({ "file_path": abs_path.to_string_lossy() }))
            .send()
            .await?);
    }
    let data = tokio::fs::read(abs_path)
        .await
        .map_err(|e| papered::PaperedError::io_other(format!("read error: {e}")))?;
    let file_name = abs_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();
    let form = reqwest::multipart::Form::new().part(
        "file",
        reqwest::multipart::Part::bytes(data).file_name(file_name),
    );
    Ok(client
        .client
        .post(client.url(API_PAPERS_IMPORT))
        .multipart(form)
        .send()
        .await?)
}

pub async fn handle_add(
    client: &DaemonClient,
    path: std::path::PathBuf,
    r#type: Option<String>,
) -> Result<()> {
    use colored::Colorize;
    use papered::client::check_response;

    let abs_path = canonicalize_or_warn(&path);
    let source = if let Some(type_str) = r#type {
        Some(type_str.parse::<DocumentSource>().map_err(|e| {
            papered::PaperedError::invalid_argument(format!(
                "Invalid type '{type_str}': {e}. Supported: pdf, markdown, text, image, tex, docx"
            ))
        })?)
    } else {
        DocumentSource::from_path(&abs_path)
    };
    match source {
        Some(DocumentSource::Pdf) => {
            let resp = import_file(client, &abs_path, DocumentSource::Pdf).await?;
            let paper: papered::paper::Paper = check_response(resp).await?.json().await?;
            println!("{}", "✓ Paper added successfully".green().bold());
            println!("  ID:    {}", paper.id.cyan());
            println!("  Title: {}", paper.title);
            println!("  Year:  {}", paper.year_string());
        }
        Some(source) => {
            let resp = import_file(client, &abs_path, source).await?;
            let result: serde_json::Value = check_response(resp).await?.json().await?;
            let label = source.as_str();
            println!("{}", format!("✓ {label} added successfully").green().bold());
            println!(
                "  ID:    {}",
                result["paper_id"].as_str().unwrap_or("unknown").cyan()
            );
            println!("  Title: {}", result["title"].as_str().unwrap_or("Unknown"));
        }
        None => {
            return Err(papered::PaperedError::invalid_argument(format!(
                "Unsupported file type: {}\n  Supported formats: pdf, md, markdown, txt, text, tex, png, jpg, jpeg, webp, gif, bmp, docx",
                abs_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("(no extension)")
            )));
        }
    }
    Ok(())
}

pub async fn handle_batch(
    client: &DaemonClient,
    path: std::path::PathBuf,
    recursive: bool,
) -> Result<()> {
    use colored::Colorize;

    let mut file_paths = Vec::new();
    if recursive {
        for entry in walkdir::WalkDir::new(&path)
            .into_iter()
            .filter_map(std::result::Result::ok)
        {
            if entry.file_type().is_file() && DocumentSource::from_path(entry.path()).is_some() {
                file_paths.push(entry.path().to_path_buf());
            }
        }
    } else if let Ok(entries) = std::fs::read_dir(&path) {
        for entry in entries.filter_map(std::result::Result::ok) {
            let p = entry.path();
            if p.is_file() && DocumentSource::from_path(&p).is_some() {
                file_paths.push(p);
            }
        }
    }
    if file_paths.is_empty() {
        println!("{}", "No supported files found.".yellow());
        println!(
            "  Supported: pdf, md, markdown, txt, text, tex, png, jpg, jpeg, webp, gif, bmp, docx"
        );
    } else {
        println!("Found {} file(s). Indexing...", file_paths.len());
        let mut success = 0;
        let mut failed = 0;
        for (i, file_path) in file_paths.iter().enumerate() {
            let abs_path = canonicalize_or_warn(file_path);
            let resp = match DocumentSource::from_path(&abs_path) {
                Some(source) => import_file(client, &abs_path, source).await,
                None => {
                    failed += 1;
                    continue;
                }
            };
            match resp {
                Ok(r) if r.status().is_success() => match r.json::<serde_json::Value>().await {
                    Ok(parsed) => {
                        let id = parsed
                            .get("id")
                            .or_else(|| parsed.get("paper_id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let title = parsed
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Unknown");
                        success += 1;
                        println!(
                            "  [{}/{}] ✓ {} - {}",
                            i + 1,
                            file_paths.len(),
                            id.cyan(),
                            title
                        );
                    }
                    Err(e) => {
                        failed += 1;
                        println!(
                            "  [{}/{}] ✗ {} - JSON parse error: {}",
                            i + 1,
                            file_paths.len(),
                            file_path.display(),
                            e
                        );
                    }
                },
                Ok(r) => {
                    failed += 1;
                    let status = r.status();
                    let text = match r.text().await {
                        Ok(t) if !t.is_empty() => t,
                        _ => format!("HTTP {status}"),
                    };
                    println!(
                        "  [{}/{}] ✗ {} - {}",
                        i + 1,
                        file_paths.len(),
                        file_path.display(),
                        text
                    );
                }
                Err(e) => {
                    failed += 1;
                    println!(
                        "  [{}/{}] ✗ {} - {}",
                        i + 1,
                        file_paths.len(),
                        file_path.display(),
                        e
                    );
                }
            }
        }
        println!(
            "\n{}",
            format!("Batch complete: {success} succeeded, {failed} failed")
                .green()
                .bold()
        );
    }
    Ok(())
}
