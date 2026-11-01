//! Model servers and providers that git-agent knows the address of.
//!
//! Every provider here speaks the OpenAI-compatible chat completions API, so
//! choosing one only sets `base_url`. Model names come from each server's own
//! model list, never from this file.

use reqwest::Url;

pub struct Provider {
    pub name: &'static str,
    pub base_url: &'static str,
    /// Runs on your own machine, perhaps at another address, and needs no API key.
    pub local: bool,
}

const fn local(name: &'static str, base_url: &'static str) -> Provider {
    Provider {
        name,
        base_url,
        local: true,
    }
}

const fn cloud(name: &'static str, base_url: &'static str) -> Provider {
    Provider {
        name,
        base_url,
        local: false,
    }
}

pub const PROVIDERS: &[Provider] = &[
    local("LM Studio", "http://localhost:1234/v1"),
    local("Ollama", "http://localhost:11434/v1"),
    local("llama.cpp", "http://localhost:8080/v1"),
    local("vLLM", "http://localhost:8000/v1"),
    cloud("OpenAI", "https://api.openai.com/v1"),
    cloud("Anthropic", "https://api.anthropic.com/v1"),
    cloud("OpenRouter", "https://openrouter.ai/api/v1"),
    cloud(
        "Google Gemini",
        "https://generativelanguage.googleapis.com/v1beta/openai",
    ),
    cloud("Mistral", "https://api.mistral.ai/v1"),
    cloud("Groq", "https://api.groq.com/openai/v1"),
    cloud("DeepSeek", "https://api.deepseek.com/v1"),
    cloud("xAI", "https://api.x.ai/v1"),
    cloud("Together AI", "https://api.together.xyz/v1"),
    cloud("Fireworks AI", "https://api.fireworks.ai/inference/v1"),
    cloud("Cerebras", "https://api.cerebras.ai/v1"),
];

/// The provider at `base_url`, if it is one of [`PROVIDERS`].
pub fn find(base_url: &str) -> Option<&'static Provider> {
    let base_url = base_url.trim_end_matches('/');
    PROVIDERS
        .iter()
        .find(|provider| provider.base_url == base_url)
}

/// Whether `base_url` points at this machine.
pub fn is_local(base_url: &str) -> bool {
    Url::parse(base_url)
        .is_ok_and(|url| matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
}

pub fn is_anthropic(base_url: &str) -> bool {
    Url::parse(base_url).is_ok_and(|url| url.host_str() == Some("api.anthropic.com"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_valid_and_distinct() {
        for (index, provider) in PROVIDERS.iter().enumerate() {
            let url = Url::parse(provider.base_url).unwrap();
            assert_eq!(url.scheme() == "http", provider.local, "{}", provider.name);
            assert!(!provider.base_url.ends_with('/'));
            let others = &PROVIDERS[index + 1..];
            assert!(others.iter().all(|other| other.name != provider.name));
            assert!(
                others
                    .iter()
                    .all(|other| other.base_url != provider.base_url)
            );
        }
    }

    #[test]
    fn finds_providers_by_url() {
        assert_eq!(find("https://api.openai.com/v1/").unwrap().name, "OpenAI");
        assert_eq!(find("http://localhost:11434/v1").unwrap().name, "Ollama");
        assert!(find("http://localhost:9999/v1").is_none());
        assert!(is_local("http://localhost:9999/v1") && is_local("http://127.0.0.1:1234/v1"));
        assert!(!is_local("https://api.openai.com/v1"));
        assert!(is_anthropic("https://api.anthropic.com/v1/"));
        assert!(!is_anthropic("https://openrouter.ai/api/v1"));
    }
}
