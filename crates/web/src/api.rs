//! Calls to the duet server's JSON API.

use gloo_net::http::{Request, Response};
use serde::{Serialize, de::DeserializeOwned};

#[derive(Debug, Clone)]
pub struct ApiError(pub String);

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

async fn decode<T: DeserializeOwned>(resp: Response) -> ApiResult<T> {
    if !resp.ok() {
        let msg = resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or_else(|| format!("HTTP {}", resp.status()));
        return Err(ApiError(msg));
    }
    // 202/204 carry no body; `null` decodes into `()` and `Option`.
    let text = resp.text().await.map_err(|e| ApiError(e.to_string()))?;
    serde_json::from_str(if text.is_empty() { "null" } else { &text }).map_err(|e| ApiError(e.to_string()))
}

pub async fn get<T: DeserializeOwned>(url: &str) -> ApiResult<T> {
    decode(Request::get(url).send().await.map_err(|e| ApiError(e.to_string()))?).await
}

pub async fn send<B: Serialize, T: DeserializeOwned>(method: &str, url: &str, body: &B) -> ApiResult<T> {
    let builder = match method {
        "PUT" => Request::put(url),
        "PATCH" => Request::patch(url),
        _ => Request::post(url),
    };
    let req = builder.json(body).map_err(|e| ApiError(e.to_string()))?;
    decode(req.send().await.map_err(|e| ApiError(e.to_string()))?).await
}

pub async fn post<B: Serialize, T: DeserializeOwned>(url: &str, body: &B) -> ApiResult<T> {
    send("POST", url, body).await
}

pub async fn put<B: Serialize, T: DeserializeOwned>(url: &str, body: &B) -> ApiResult<T> {
    send("PUT", url, body).await
}

pub async fn remove(url: &str) -> ApiResult<()> {
    decode(Request::delete(url).send().await.map_err(|e| ApiError(e.to_string()))?).await
}
