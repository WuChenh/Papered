#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Ollama,
    OpenAiCompatible,
    Qianwen,
}

impl Provider {
    pub fn from_url(api_base: &str) -> Self {
        let lower = api_base.to_lowercase();
        if lower.contains("/api/generate")
            || lower.contains("/api/chat")
            || lower.ends_with("/api")
            || lower.contains("ollama")
            || lower.contains(":11434")
        {
            Provider::Ollama
        } else if lower.contains("qianwen") || lower.contains("dashscope") {
            Provider::Qianwen
        } else {
            Provider::OpenAiCompatible
        }
    }

    #[must_use]
    pub fn chat_path(&self) -> &'static str {
        match self {
            Provider::Ollama => "/api/chat",
            Provider::OpenAiCompatible | Provider::Qianwen => "/chat/completions",
        }
    }

    #[must_use]
    pub fn embedding_path(&self) -> &'static str {
        match self {
            Provider::Ollama => "/api/embed",
            Provider::OpenAiCompatible | Provider::Qianwen => "/embeddings",
        }
    }

    #[must_use]
    pub fn rerank_path(&self) -> &'static str {
        match self {
            Provider::Qianwen => "/reranks",
            _ => "/rerank",
        }
    }

    /// Ollama-specific `/api/generate` path.
    /// Returns `None` for non-Ollama providers because the generate endpoint is Ollama-specific.
    #[must_use]
    pub fn generate_path(&self) -> Option<&'static str> {
        match self {
            Provider::Ollama => Some("/api/generate"),
            _ => None,
        }
    }

    /// Join a provider path onto an API base URL, normalizing trailing slashes.
    #[must_use]
    pub fn build_url(&self, api_base: &str, path: &str) -> String {
        format!("{}{}", api_base.trim_end_matches('/'), path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_ollama_by_name() {
        assert_eq!(
            Provider::from_url("http://localhost:11434"),
            Provider::Ollama
        );
        assert_eq!(
            Provider::from_url("http://my-ollama-server:11434"),
            Provider::Ollama
        );
    }

    #[test]
    fn detects_ollama_by_api_path() {
        assert_eq!(
            Provider::from_url("http://localhost:11434/api/generate"),
            Provider::Ollama
        );
        assert_eq!(
            Provider::from_url("http://localhost:11434/api/chat"),
            Provider::Ollama
        );
        assert_eq!(
            Provider::from_url("http://localhost:11434/api"),
            Provider::Ollama
        );
    }

    #[test]
    fn detects_qianwen() {
        assert_eq!(
            Provider::from_url("https://dashscope.aliyuncs.com"),
            Provider::Qianwen
        );
    }

    #[test]
    fn defaults_to_openai_compatible() {
        assert_eq!(
            Provider::from_url("https://api.openai.com/v1"),
            Provider::OpenAiCompatible
        );
    }

    #[test]
    fn paths_are_correct() {
        assert_eq!(Provider::Ollama.chat_path(), "/api/chat");
        assert_eq!(Provider::OpenAiCompatible.chat_path(), "/chat/completions");
        assert_eq!(Provider::Qianwen.chat_path(), "/chat/completions");
        assert_eq!(Provider::Ollama.embedding_path(), "/api/embed");
        assert_eq!(Provider::OpenAiCompatible.embedding_path(), "/embeddings");
        assert_eq!(Provider::Qianwen.embedding_path(), "/embeddings");
        assert_eq!(Provider::Ollama.rerank_path(), "/rerank");
        assert_eq!(Provider::OpenAiCompatible.rerank_path(), "/rerank");
        assert_eq!(Provider::Qianwen.rerank_path(), "/reranks");
        assert_eq!(Provider::Ollama.generate_path(), Some("/api/generate"));
        assert_eq!(Provider::OpenAiCompatible.generate_path(), None);
        assert_eq!(Provider::Qianwen.generate_path(), None);
    }

    #[test]
    fn build_url_normalizes_trailing_slashes() {
        let p = Provider::OpenAiCompatible;
        assert_eq!(
            p.build_url("https://api.openai.com/v1", "/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            p.build_url("https://api.openai.com/v1/", "/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            p.build_url("https://api.openai.com/v1//", "/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn detects_provider_case_insensitively() {
        assert_eq!(
            Provider::from_url("https://DASHSCOPE.aliyuncs.com"),
            Provider::Qianwen
        );
        assert_eq!(
            Provider::from_url("http://LOCALHOST:11434/OLLAMA"),
            Provider::Ollama
        );
    }

    #[test]
    fn handles_trailing_v1() {
        assert_eq!(
            Provider::from_url("https://api.openai.com/v1"),
            Provider::OpenAiCompatible
        );
        assert_eq!(
            Provider::from_url("https://api.openai.com/v1/"),
            Provider::OpenAiCompatible
        );
    }

    #[test]
    fn unknown_host_defaults_to_openai_compatible() {
        assert_eq!(
            Provider::from_url("https://example.com"),
            Provider::OpenAiCompatible
        );
        assert_eq!(
            Provider::from_url("https://foo.bar/baz"),
            Provider::OpenAiCompatible
        );
    }
}
