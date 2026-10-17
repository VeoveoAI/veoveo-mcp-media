//! Registration with the Hub: Media's key, its declarations, and the client that
//! removes the registration when Media stops.
use anyhow::{Context, Result};
use veoveo_hub_sdk::registration::{
    ClientMetadata, RegistrationDeclaration,
    client::{KeyDirectory, Registration, RegistrationClient},
    statement::ServerKeyPair,
};

use super::config::Args;
use crate::mcp::AUTHORIZATION;

/// A registration and what calls to other servers need from it.
pub(super) struct Registered {
    pub client: RegistrationClient,
    pub registration: Registration,
    pub metadata: ClientMetadata,
    pub key: ServerKeyPair,
    /// The HTTP client that reached the Hub, which also fetches the Hub's token
    /// keys when a forwarded token names a key the registration lacks.
    pub http: reqwest::Client,
}

/// Register Media, declaring the Artifact permissions it uses.
pub(super) async fn register(
    args: &Args,
    declaration: RegistrationDeclaration,
) -> Result<Registered> {
    let keys = KeyDirectory::new(&args.key_directory);
    let key = keys
        .load_or_generate()
        .context("loading Media's registration key")?;
    let metadata = ClientMetadata::new(*key.public_key(), declaration, AUTHORIZATION.clone())?;
    let http = reqwest::Client::builder()
        .build()
        .context("building the registration HTTP client")?;
    let endpoint = RegistrationClient::discover(&http, &args.hub_issuer)
        .await
        .context("discovering the Hub's registration endpoint")?;
    let client = RegistrationClient::new(
        http.clone(),
        endpoint,
        args.registration_secret.clone(),
        key,
        metadata.clone(),
    )?;
    let registration = client
        .register()
        .await
        .context("registering Media with the Hub")?;
    tracing::info!(client_id = %registration.client_id(), "registered with the Hub");
    // The same key signs the client assertions of Media's token exchanges.
    let key = keys
        .load_or_generate()
        .context("loading Media's registration key")?;
    Ok(Registered {
        client,
        registration,
        metadata,
        key,
        http,
    })
}
