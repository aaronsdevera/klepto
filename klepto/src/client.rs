//! Thin HTTP client used by the terminal frontend.

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::config::Config;

#[derive(Clone)]
pub struct ApiClient {
    base: String,
    http: reqwest::Client,
    token: Option<String>,
}

impl ApiClient {
    pub fn from_config(config: &Config) -> Self {
        let listen = config.listen.trim();
        let base = if listen.starts_with("http://") || listen.starts_with("https://") {
            format!(
                "{}/v1",
                listen.trim_end_matches('/').trim_end_matches("/v1")
            )
        } else {
            format!("http://{listen}/v1")
        };
        Self {
            base,
            http: reqwest::Client::new(),
            token: config.token.clone(),
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let request = self.authorize(self.http.get(format!("{}{}", self.base, path)));
        let response = request.send().await.map_err(connection_error)?;
        decode(response).await
    }

    pub async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, String> {
        let request = self.authorize(self.http.post(format!("{}{}", self.base, path)).json(body));
        let response = request.send().await.map_err(connection_error)?;
        decode(response).await
    }

    pub async fn delete<T: DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let request = self.authorize(self.http.delete(format!("{}{}", self.base, path)));
        let response = request.send().await.map_err(connection_error)?;
        decode(response).await
    }

    fn authorize(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.token.as_deref() {
            Some(token) if !token.is_empty() => request.bearer_auth(token),
            _ => request,
        }
    }
}

async fn decode<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, String> {
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("read daemon response: {e}"))?;
    if !status.is_success() {
        let detail = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .or_else(|| value.get("message"))
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| String::from_utf8_lossy(&bytes).to_string());
        return Err(format!("daemon returned HTTP {status}: {detail}"));
    }
    serde_json::from_slice(&bytes).map_err(|e| format!("decode daemon response: {e}"))
}

fn connection_error(error: reqwest::Error) -> String {
    format!(
        "cannot reach Klepto daemon: {error}. Start it with `klepto serve` or install the user service."
    )
}
