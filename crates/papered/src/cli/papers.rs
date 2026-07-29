use papered::client::DaemonClient;
use papered::error::Result;
use papered::routes::API_PAPERS;

pub async fn handle_list(client: &DaemonClient, limit: usize, offset: usize) -> Result<()> {
    use colored::Colorize;
    use papered::client::api_get_json;

    let list_response: papered::ListPapersResponse = api_get_json(
        client,
        &format!("{API_PAPERS}?limit={limit}&offset={offset}"),
    )
    .await?;
    let papers = list_response.papers;
    if papers.is_empty() {
        println!("{}", "No papers in your thought space.".yellow());
    } else {
        println!(
            "{} {} paper(s) in thought space\n",
            "✓".green().bold(),
            list_response.total
        );
        for paper in papers {
            println!(
                "  {} {} ({}, {})",
                paper.id.cyan(),
                paper.title.bold(),
                paper.year_string(),
                paper.authors_string().dimmed()
            );
        }
    }
    Ok(())
}

pub async fn handle_show(client: &DaemonClient, paper_id: String) -> Result<()> {
    use colored::Colorize;
    use papered::client::{api_get_json, api_request, check_response};

    let resp = api_request(
        client,
        reqwest::Method::GET,
        &format!("{API_PAPERS}/{paper_id}"),
        None,
    )
    .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        println!("{}", format!("Paper not found: {paper_id}").red());
        return Ok(());
    }
    let resp = check_response(resp).await?;
    let paper: papered::paper::Paper = resp.json().await?;

    println!("{}", "Paper Details".bold().underline());
    println!("  ID:       {}", paper.id.cyan());
    println!("  Title:    {}", paper.title.bold());
    println!("  Authors:  {}", paper.authors_string());
    println!("  Year:     {}", paper.year_string());
    println!("  Venue:    {}", paper.venue.as_deref().unwrap_or("N/A"));
    println!("  DOI:      {}", paper.doi.as_deref().unwrap_or("N/A"));
    println!("  Keywords: {}", paper.keywords_string());
    if !paper.affiliations.is_empty() {
        println!("  Affiliations: {}", paper.affiliations.join(", "));
    }
    if !paper.emails.is_empty() {
        println!("  Emails: {}", paper.emails.join(", "));
    }
    if let Some(ref extra) = paper.extra {
        println!("  Extra: {extra}");
    }
    println!(
        "  File:     {}",
        paper.file_path.as_deref().unwrap_or("N/A")
    );

    if !paper.entities.is_empty() {
        println!("\n{}", "Bio-entities".bold().underline());
        let print_entities = |label: &str, values: &[String]| {
            if !values.is_empty() {
                println!("  {}: {}", label.cyan(), values.join(", "));
            }
        };
        print_entities("Species", &paper.entities.species);
        print_entities("Genes", &paper.entities.genes);
        print_entities("Techniques", &paper.entities.techniques);
        print_entities("Pathways", &paper.entities.pathways);
    }

    if let Ok(sections) = api_get_json::<papered::paper::section::PaperSections>(
        client,
        &format!("{API_PAPERS}/{paper_id}/sections"),
    )
    .await
        && !sections.sections.is_empty()
    {
        println!("\n{}", "Sections".bold().underline());
        for section in &sections.sections {
            let content =
                papered::util::truncate_chars(&section.content, super::SECTION_DISPLAY_MAX_CHARS);
            println!(
                "  {}: {content}",
                section.section_type.to_string().magenta().bold()
            );
        }
    }
    Ok(())
}

pub async fn handle_delete(client: &DaemonClient, paper_id: String) -> Result<()> {
    use colored::Colorize;
    use papered::client::{api_request, check_response};

    let resp = api_request(
        client,
        reqwest::Method::DELETE,
        &format!("{API_PAPERS}/{paper_id}"),
        None,
    )
    .await?;
    check_response(resp).await?;
    println!("{}", format!("✓ Paper {paper_id} deleted").green());
    Ok(())
}
