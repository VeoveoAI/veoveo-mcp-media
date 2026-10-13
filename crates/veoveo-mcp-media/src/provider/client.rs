//! The generation provider's HTTP API: its model registry, submissions, billing records
//! and cancellations. Every call carries a timeout, and no error names a URL, because a
//! submission's URL carries the callback's bearer token.
use std::time::Duration;

use reqwest::{StatusCode, Url};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use super::wire::{
    BillingQuery, BillingRecord, BillingSearch, CancellationReceipt, CancellationRequest, Envelope,
    ModelEntry, Prediction,
};
use crate::contract::{MediaModelId, MediaPredictionId};

/// How long reading the model registry may take.
pub const REGISTRY_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a submission may take to be accepted.
pub const SUBMIT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a billing search may take.
pub const BILLING_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a cancellation may take.
pub const CANCEL_TIMEOUT: Duration = Duration::from_secs(10);
/// How long reading one prediction's status may take.
pub const STATUS_TIMEOUT: Duration = Duration::from_secs(15);

/// Why a provider call failed. None of these names the URL.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error(
        "the provider address must be an http or https URL with a host and no credentials, query or fragment"
    )]
    InvalidEndpoint,
    #[error("the provider could not be reached: {0}")]
    Transport(String),
    #[error("the provider answered HTTP {0}")]
    Status(u16),
    #[error("the provider refused the request with code {0}")]
    Api(i64),
    #[error("the provider's answer could not be read: {0}")]
    Decode(String),
    #[error("the prediction's status endpoint lies outside the provider's origin")]
    ForeignStatusUrl,
}

impl ProviderError {
    fn transport(error: reqwest::Error) -> Self {
        Self::Transport(error.without_url().to_string())
    }

    /// Whether the call may have reached the provider without an answer, so its effect is
    /// unknown.
    pub fn is_ambiguous(&self) -> bool {
        matches!(self, Self::Transport(_)) || matches!(self, Self::Status(status) if *status >= 500)
    }
}

/// The provider's API at a configured address with the installation's API key.
#[derive(Clone)]
pub struct ProviderClient {
    http: reqwest::Client,
    base: Url,
    api_key: SecretString,
}

impl std::fmt::Debug for ProviderClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderClient")
            .field("base", &self.base.as_str())
            .field("api_key", &"redacted")
            .finish_non_exhaustive()
    }
}

impl ProviderClient {
    /// The provider at `base`. There is no default address: every installation and test
    /// names the provider it calls.
    pub fn new(
        http: reqwest::Client,
        base: Url,
        api_key: SecretString,
    ) -> Result<Self, ProviderError> {
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(ProviderError::InvalidEndpoint);
        }
        Ok(Self {
            http,
            base,
            api_key,
        })
    }

    /// The provider's host, and its host and port, as allowed output hosts spell them.
    pub fn host(&self) -> Option<(String, String)> {
        let host = self.base.host_str()?.to_ascii_lowercase();
        let with_port = match self.base.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.clone(),
        };
        Some((host, with_port))
    }

    fn endpoint<'a>(&self, components: impl IntoIterator<Item = &'a str>) -> Url {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .expect("an admitted http URL has a path")
            .pop_if_empty()
            .extend(["api", "v3"])
            .extend(components);
        url
    }

    async fn answer<T: DeserializeOwned>(
        response: Result<reqwest::Response, reqwest::Error>,
    ) -> Result<T, ProviderError> {
        let response = response.map_err(ProviderError::transport)?;
        let status = response.status();
        if !status.is_success() {
            return Err(ProviderError::Status(status.as_u16()));
        }
        let bytes = response.bytes().await.map_err(ProviderError::transport)?;
        let envelope: Envelope<T> = serde_json::from_slice(&bytes)
            .map_err(|error| ProviderError::Decode(error.to_string()))?;
        if envelope.code != i64::from(StatusCode::OK.as_u16()) {
            return Err(ProviderError::Api(envelope.code));
        }
        Ok(envelope.data)
    }

    /// The provider's whole model registry.
    pub async fn models(&self) -> Result<Vec<ModelEntry>, ProviderError> {
        Self::answer(
            self.http
                .get(self.endpoint(["models"]))
                .bearer_auth(self.api_key.expose_secret())
                .timeout(REGISTRY_TIMEOUT)
                .send()
                .await,
        )
        .await
    }

    /// Submit a run of `model`. The provider posts the prediction's outcome to `webhook`,
    /// which it receives as the `webhook` query parameter.
    pub async fn submit(
        &self,
        model: &MediaModelId,
        input: &Map<String, Value>,
        webhook: Option<&Url>,
    ) -> Result<Prediction, ProviderError> {
        let mut url = self.endpoint(model.components());
        if let Some(webhook) = webhook {
            url.query_pairs_mut()
                .append_pair("webhook", webhook.as_str());
        }
        Self::answer(
            self.http
                .post(url)
                .bearer_auth(self.api_key.expose_secret())
                .json(input)
                .timeout(SUBMIT_TIMEOUT)
                .send()
                .await,
        )
        .await
    }

    /// The billing records of one prediction.
    pub async fn billing(
        &self,
        prediction: &MediaPredictionId,
    ) -> Result<Vec<BillingRecord>, ProviderError> {
        let search: BillingSearch = Self::answer(
            self.http
                .post(self.endpoint(["billings", "search"]))
                .bearer_auth(self.api_key.expose_secret())
                .json(&BillingQuery {
                    page: 1,
                    page_size: 100,
                    prediction_uuids: vec![prediction.clone()],
                    sort: "created_at ASC".into(),
                })
                .timeout(BILLING_TIMEOUT)
                .send()
                .await,
        )
        .await?;
        Ok(search.items)
    }

    /// One prediction's current state at its status endpoint. The endpoint must lie on
    /// the provider's own origin, so a prediction never sends Media's API key elsewhere.
    pub async fn status(&self, status_url: &str) -> Result<Prediction, ProviderError> {
        let url = Url::parse(status_url).map_err(|_| ProviderError::ForeignStatusUrl)?;
        if url.scheme() != self.base.scheme()
            || url.host_str() != self.base.host_str()
            || url.port_or_known_default() != self.base.port_or_known_default()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(ProviderError::ForeignStatusUrl);
        }
        Self::answer(
            self.http
                .get(url)
                .bearer_auth(self.api_key.expose_secret())
                .timeout(STATUS_TIMEOUT)
                .send()
                .await,
        )
        .await
    }

    /// Ask the provider to delete one prediction. The provider documents deletion, not a
    /// stop of compute or a refund, so the receipt only acknowledges the request.
    pub async fn cancel(
        &self,
        prediction: &MediaPredictionId,
    ) -> Result<CancellationReceipt, ProviderError> {
        Self::answer(
            self.http
                .post(self.endpoint(["predictions", "delete"]))
                .bearer_auth(self.api_key.expose_secret())
                .json(&CancellationRequest {
                    ids: vec![prediction.clone()],
                })
                .timeout(CANCEL_TIMEOUT)
                .send()
                .await,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(base: &str) -> Result<ProviderClient, ProviderError> {
        ProviderClient::new(
            reqwest::Client::new(),
            Url::parse(base).unwrap(),
            SecretString::from("request-secret"),
        )
    }

    #[test]
    fn the_address_admits_plain_http_origins_only() {
        assert!(client("https://provider.example").is_ok());
        assert!(client("http://127.0.0.1:8803/base/").is_ok());
        for base in [
            "ftp://provider.example",
            "https://user:pass@provider.example",
            "https://provider.example/?a=b",
            "https://provider.example/#x",
        ] {
            assert!(
                matches!(client(base), Err(ProviderError::InvalidEndpoint)),
                "{base}"
            );
        }
        let rendered = format!("{:?}", client("https://provider.example").unwrap());
        assert!(!rendered.contains("request-secret"));
    }

    #[test]
    fn endpoints_extend_the_base_path() {
        let client = client("http://127.0.0.1:8803/base/").unwrap();
        let model = MediaModelId::parse("owner/model").unwrap();
        assert_eq!(
            client.endpoint(model.components()).as_str(),
            "http://127.0.0.1:8803/base/api/v3/owner/model"
        );
        assert_eq!(
            client.endpoint(["billings", "search"]).as_str(),
            "http://127.0.0.1:8803/base/api/v3/billings/search"
        );
    }

    #[tokio::test]
    async fn a_status_endpoint_off_the_providers_origin_is_never_called() {
        let client = client("http://127.0.0.1:8803").unwrap();
        for url in [
            "http://elsewhere.example:8803/api/v3/predictions/p/result",
            "http://127.0.0.1:9999/api/v3/predictions/p/result",
            "https://127.0.0.1:8803/api/v3/predictions/p/result",
            "http://user@127.0.0.1:8803/x",
            "not a url",
        ] {
            assert!(
                matches!(
                    client.status(url).await,
                    Err(ProviderError::ForeignStatusUrl)
                ),
                "{url}"
            );
        }
    }

    #[tokio::test]
    async fn errors_name_no_url_or_secret() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        // Nothing listens on this port once the listener is dropped.
        let address = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let client = client(&format!("http://{address}")).unwrap();
        let callback =
            Url::parse("http://media.test/media/operations/op/callback-secret-sentinel").unwrap();
        let error = client
            .submit(
                &MediaModelId::parse("m/x").unwrap(),
                &Map::new(),
                Some(&callback),
            )
            .await
            .unwrap_err();
        assert!(error.is_ambiguous());
        for rendered in [error.to_string(), format!("{error:?}")] {
            assert!(!rendered.contains("callback-secret-sentinel"), "{rendered}");
            assert!(!rendered.contains("request-secret"), "{rendered}");
        }
    }
}
