//! Blocking HTTP GET with timeouts, for the polling threads.

use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(15);
/// Yahoo answers requests without a browser-like User-Agent with 429.
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) sidebar/0.1";

pub(crate) struct Http {
    agent: ureq::Agent,
}

impl Http {
    pub(crate) fn new() -> Self {
        let config = ureq::Agent::config_builder().timeout_global(Some(TIMEOUT)).user_agent(USER_AGENT).build();
        Self { agent: config.into() }
    }

    /// The response body; HTTP errors (4xx, 5xx) are errors.
    pub(crate) fn get(&self, url: &str) -> Result<String, String> {
        let mut response = self.agent.get(url).call().map_err(|e| format!("request failed: {e}"))?;
        response.body_mut().read_to_string().map_err(|e| format!("reading response: {e}"))
    }
}

/// Percent-encodes a URL path segment or query value.
pub(crate) fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}
