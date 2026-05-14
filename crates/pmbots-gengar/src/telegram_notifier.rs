//! Telegram notifier — fire-and-forget POSTs to sendMessage.
//! Reference: telegram_notifier.py.

use serde_json::json;
use tracing::warn;

#[derive(Debug, Clone)]
pub struct Telegram {
    bot_token: String,
    chat_id: String,
    http: reqwest::Client,
}

impl Telegram {
    /// Build from env. Returns None if either var is missing/empty.
    pub fn from_env() -> Option<Self> {
        let bot_token = std::env::var("GENGAR_TELEGRAM_BOT_TOKEN").ok().filter(|s| !s.is_empty())?;
        let chat_id   = std::env::var("GENGAR_TELEGRAM_CHAT_ID").ok().filter(|s| !s.is_empty())?;
        Some(Self { bot_token, chat_id, http: reqwest::Client::new() })
    }

    /// Send a Markdown-formatted message. Errors are logged and swallowed.
    pub fn send(&self, text: &str) {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token);
        let body = json!({
            "chat_id": self.chat_id,
            "text": text,
            "parse_mode": "Markdown",
        });
        let http = self.http.clone();
        tokio::spawn(async move {
            if let Err(e) = http.post(&url).json(&body).send().await {
                warn!("[GENGAR][TG] send failed: {}", e);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_env_returns_none_when_missing() {
        std::env::remove_var("GENGAR_TELEGRAM_BOT_TOKEN");
        std::env::remove_var("GENGAR_TELEGRAM_CHAT_ID");
        assert!(Telegram::from_env().is_none());
    }
}
