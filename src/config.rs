/// Runtime configuration loaded from the environment (via `.env`).
#[derive(Debug, Clone)]
pub struct Config {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
}

impl Config {
    /// Load required `REM_*` vars. Missing vars are a hard error naming the var.
    pub fn from_env() -> Result<Self, String> {
        // Load `.env` if present; ignore errors (env may be set directly).
        dotenvy::dotenv().ok();

        fn required(var: &str) -> Result<String, String> {
            std::env::var(var)
                .map_err(|_| format!("missing required environment variable: {var}"))
        }

        Ok(Self {
            api_key: required("REM_API_KEY")?,
            base_url: required("REM_BASE_URL")?,
            model: required("REM_MODEL")?,
        })
    }
}
