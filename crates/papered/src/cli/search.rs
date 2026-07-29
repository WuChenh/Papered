use papered::client::DaemonClient;
use papered::error::Result;

pub async fn handle_search(
    client: &DaemonClient,
    query: String,
    section: Option<String>,
    limit: usize,
    min_score: f32,
    search_method: Option<String>,
) -> Result<()> {
    use colored::Colorize;
    use papered::client::api_post_json;
    use papered::routes::API_SEARCH;

    let results: Vec<papered::paper::PaperSearchResult> = api_post_json(
        client, API_SEARCH,
        Some(serde_json::json!({
            "query": query, "section_type": section, "limit": limit, "min_score": min_score, "search_method": search_method,
        })),
    ).await?;
    if results.is_empty() {
        println!("{}", "No results found.".yellow());
    } else {
        println!(
            "{} {} result(s) for: \"{}\"\n",
            "✓".green().bold(),
            results.len(),
            query.bold()
        );
        for (i, result) in results.iter().enumerate() {
            print_result(i + 1, result);
        }
    }
    Ok(())
}

pub async fn handle_similar(
    client: &DaemonClient,
    paper_id: String,
    section: Option<String>,
    limit: usize,
) -> Result<()> {
    use colored::Colorize;
    use papered::client::api_post_json;
    use papered::routes::API_SIMILAR;

    let results: Vec<papered::paper::PaperSearchResult> = api_post_json(
        client,
        API_SIMILAR,
        Some(serde_json::json!({ "paper_id": paper_id, "section_type": section, "limit": limit })),
    )
    .await?;
    if results.is_empty() {
        println!("{}", "No similar papers found.".yellow());
    } else {
        println!(
            "{} {} similar paper(s)\n",
            "✓".green().bold(),
            results.len()
        );
        for (i, result) in results.iter().enumerate() {
            print_result(i + 1, result);
        }
    }
    Ok(())
}

pub fn print_result(rank: usize, result: &papered::paper::PaperSearchResult) {
    use colored::Colorize;

    println!(
        "{}. {} {} (score: {:.3})",
        rank,
        result.paper.title.bold(),
        format!("[{}]", result.paper.year_string()).dimmed(),
        result.score
    );
    println!("   {}", result.paper.authors_string().dimmed());

    if !result.matched_sections.is_empty() {
        for section in &result.matched_sections {
            println!(
                "   {} {}: {}",
                "→".cyan(),
                section.section_type.magenta(),
                section.content_snippet.dimmed()
            );
        }
    }
    println!();
}
