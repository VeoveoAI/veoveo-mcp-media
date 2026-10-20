//! Registration with the Hub: Media's key, its declarations, and the client that
//! resumes native saved registration across process restarts.
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
    let metadata = prepare(args, declaration)?;
    let key = keys
        .load_or_generate()
        .context("loading Media's registration key")?;
    let http = reqwest::Client::builder()
        .build()
        .context("building the registration HTTP client")?;
    let endpoint = RegistrationClient::discover(
        &http,
        args.hub_issuer
            .as_ref()
            .context("serving requires a Hub issuer")?,
    )
    .await
    .context("discovering the Hub's registration endpoint")?;
    let client = RegistrationClient::new(
        http.clone(),
        endpoint,
        args.registration_secret
            .clone()
            .context("serving requires initial registration admission")?,
        key,
        metadata.clone(),
    )?;
    let registration = client
        .resume_or_register(&keys)
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

/// Identical metadata construction for offline admission and actual serving.
/// This synchronous boundary only reads/writes the native key directory.
pub(super) fn prepare(args: &Args, declaration: RegistrationDeclaration) -> Result<ClientMetadata> {
    KeyDirectory::new(&args.key_directory)
        .prepare(declaration, AUTHORIZATION.clone())
        .context("preparing Media's registration metadata")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn offline_prepare_reuses_worker_key_and_creates_no_registration_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let args = Args::try_parse_from([
            "veoveo-mcp-media",
            "--describe-registration",
            "--deployment-id",
            "019a5b30-0000-7000-8000-000000000001",
            "--public-base-url",
            "https://media.example",
            "--key-directory",
            directory.path().to_str().unwrap(),
        ])
        .unwrap();
        let declaration =
            crate::mcp::registration(&args.public_deployment().unwrap(), args.deployment_id)
                .unwrap();
        let first = prepare(&args, declaration.clone()).unwrap();
        assert_eq!(prepare(&args, declaration).unwrap(), first);
        assert_eq!(first.registration.deployment, args.deployment_id);
        assert!(
            !KeyDirectory::new(directory.path())
                .registration_path()
                .exists()
        );
        let metadata = serde_json::to_value(first).unwrap();
        assert!(metadata.get("veoveo_database").is_none());
        assert!(metadata.get("registration_access_token").is_none());
    }
}
