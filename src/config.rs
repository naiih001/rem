use serde::Deserialize;

/// Top-level config — composed of domain-specific sub-structs.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub api: ApiConfig,
    pub model: ModelConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiConfig {
    pub key: String,
    pub base_url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelConfig {
    pub default: String,
    /// Reasoning effort: `low` / `medium` / `high`.
    /// Defaults to `medium` if unset or unrecognized.
    #[serde(default = "default_effort")]
    pub effort: String,
}

fn default_effort() -> String {
    "medium".to_string()
}

impl Config {
    /// Load config from `~/.config/rem/config.toml`.
    pub fn from_file() -> Result<Self, String> {
        let config_dir =
            dirs::config_dir().ok_or("could not determine platform config directory")?;
        let config_path = config_dir.join("rem").join("config.toml");

        let content = std::fs::read_to_string(&config_path)
            .map_err(|e| format!("failed to read {}: {e}", config_path.display()))?;

        let config: Config = toml::from_str(&content)
            .map_err(|e| format!("failed to parse {}: {e}", config_path.display()))?;

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_parses_valid_toml() {
        let toml_str = r#"
[api]
key = "sk-test"
base_url = "https://api.example.com/v1"

[model]
default = "gpt-4o"
effort = "high"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.api.key, "sk-test");
        assert_eq!(config.api.base_url, "https://api.example.com/v1");
        assert_eq!(config.model.default, "gpt-4o");
        assert_eq!(config.model.effort, "high");
    }

    #[test]
    fn effort_defaults_to_medium_when_missing() {
        let toml_str = r#"
[api]
key = "sk-test"
base_url = "https://api.example.com/v1"

[model]
default = "gpt-4o"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.model.effort, "medium");
    }

    #[test]
    fn config_fails_on_missing_required_fields() {
        let toml_str = r#"
[api]
key = "sk-test"
"#;
        let result: Result<Config, _> = toml::from_str(toml_str);
        assert!(result.is_err());
    }
}
