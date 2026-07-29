//! Built-in MCP prompt templates for RAG workflows.

use rmcp::model::{GetPromptResult, PromptMessage, Role};
use serde_json::Value;

/// Get the description for a built-in prompt by name.
fn builtin_description(name: &str) -> Option<&'static str> {
    match name {
        "search-papers" => Some("Search papers and answer based on results"),
        "literature-review" => Some("Generate a literature review on a topic"),
        _ => None,
    }
}

/// List all built-in prompt definitions.
pub fn list_builtin_prompts() -> Vec<rmcp::model::Prompt> {
    vec![
        rmcp::model::Prompt::new(
            "search-papers",
            builtin_description("search-papers"),
            Some(vec![
                rmcp::model::PromptArgument::new("query")
                    .with_description("Research question")
                    .with_required(true),
            ]),
        ),
        rmcp::model::Prompt::new(
            "literature-review",
            builtin_description("literature-review"),
            Some(vec![
                rmcp::model::PromptArgument::new("topic")
                    .with_description("Research topic")
                    .with_required(true),
                rmcp::model::PromptArgument::new("max_papers")
                    .with_description("Max papers to include")
                    .with_required(false),
            ]),
        ),
    ]
}

/// Get a built-in prompt by name with argument substitution.
pub fn get_builtin_prompt(name: &str, args: &Value) -> Option<GetPromptResult> {
    match name {
        "search-papers" => {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("the research topic");
            Some(
                GetPromptResult::new(vec![PromptMessage::new_text(
                    Role::User,
                    format!(
                        "Search for papers related to: {query}\n\n\
                         Use the `search_papers` tool to find relevant papers. \
                         After receiving the results, synthesize them into a concise \
                         answer with citations to the papers found. \
                         Focus on the most relevant papers and highlight key findings, \
                         methodologies, and conclusions."
                    ),
                )])
                .with_description(builtin_description(name).unwrap_or_default()),
            )
        }
        "literature-review" => {
            let topic = args
                .get("topic")
                .and_then(|v| v.as_str())
                .unwrap_or("the research topic");
            let max_papers = args
                .get("max_papers")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(10)
                .clamp(1, 50);
            Some(
                GetPromptResult::new(vec![PromptMessage::new_text(
                    Role::User,
                    format!(
                        "Generate a literature review on the topic: {topic}\n\n\
                         Use `search_papers` with broad queries to discover relevant papers. \
                         Collect up to {max_papers} papers on this topic. \
                         Then write a structured literature review with the following sections:\n\n\
                         1. **Introduction** - Context and importance of the topic\n\
                         2. **Thematic Clusters** - Group papers by common themes or approaches\n\
                         3. **Key Findings** - Summarize major results across papers\n\
                         4. **Methodologies** - Compare methodologies used\n\
                         5. **Gaps and Future Directions** - Identify open questions\n\
                         6. **References** - List all cited papers with full citations\n\n\
                         For each paper, include its title, authors, venue, date, and key contributions. \
                         Critically evaluate the literature rather than just summarizing."
                    ),
                )])
                .with_description(builtin_description(name).unwrap_or_default()),
            )
        }
        _ => None,
    }
}
