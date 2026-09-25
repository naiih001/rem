/// Runtime configuration loaded from the environment (via `.env`).
#[derive(Debug, Clone)]
pub struct Config {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    /// Reasoning effort level (`low` / `medium` / `high`). Optional: unset
    /// or unrecognized values fall back to `medium` — never a hard error.
    pub effort: String,
}

impl Config {
    /// Load `REM_*` vars. `REM_API_KEY`, `REM_BASE_URL`, and `REM_MODEL` are
    /// required (missing vars are a hard error naming the var); `REM_EFFORT`
    /// is optional and defaults to `medium`.
    pub fn from_env() -> Result<Self, String> {
        // Load `.env` if present; ignore errors (env may be set directly).
        dotenvy::dotenv().ok();

        fn required(var: &str) -> Result<String, String> {
            std::env::var(var).map_err(|_| format!("missing required environment variable: {var}"))
        }

        Ok(Self {
            api_key: required("REM_API_KEY")?,
            base_url: required("REM_BASE_URL")?,
            model: required("REM_MODEL")?,
            effort: parse_effort(std::env::var("REM_EFFORT").ok()),
        })
    }
}

/// Normalize `REM_EFFORT`: `low` / `medium` / `high` (case-insensitive,
/// surrounding whitespace ignored). Anything else — unset, empty, or
/// unrecognized — falls back to `medium`. No fail-hards on this var.
fn parse_effort(raw: Option<String>) -> String {
    match raw.unwrap_or_default().trim().to_lowercase().as_str() {
        "low" => "low".to_string(),
        "high" => "high".to_string(),
        _ => "medium".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effort_defaults_to_medium_when_unset_or_empty() {
        assert_eq!(parse_effort(None), "medium");
        assert_eq!(parse_effort(Some(String::new())), "medium");
        assert_eq!(parse_effort(Some("   ".to_string())), "medium");
    }

    #[test]
    fn effort_accepts_low_medium_high_case_insensitively() {
        assert_eq!(parse_effort(Some("low".to_string())), "low");
        assert_eq!(parse_effort(Some("MEDIUM".to_string())), "medium");
        assert_eq!(parse_effort(Some(" High ".to_string())), "high");
    }

    #[test]
    fn effort_falls_back_to_medium_when_unrecognized() {
        assert_eq!(parse_effort(Some("ultra".to_string())), "medium");
        assert_eq!(parse_effort(Some("max".to_string())), "medium");
    }
}
