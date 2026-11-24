use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Anthropic,
    OpenAi,
    Gemini,
    Vertex,
    AzureOpenAi,
    Copilot,
    OpenAiCompatible,
    Bedrock,
    Unknown,
    None,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
            Provider::OpenAi => "openai",
            Provider::Gemini => "gemini",
            Provider::Vertex => "vertex",
            Provider::AzureOpenAi => "azure_openai",
            Provider::Copilot => "copilot",
            Provider::OpenAiCompatible => "openai_compatible",
            Provider::Bedrock => "bedrock",
            Provider::Unknown => "unknown",
            Provider::None => "none",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_are_stable() {
        assert_eq!(Provider::AzureOpenAi.as_str(), "azure_openai");
        assert_eq!(Provider::None.as_str(), "none");
    }
}
