//! The server's configuration. Every flag has its environment variable. The Gateway
//! issues the database login and the access-token trust when Media registers, so neither
//! is configured here. The provider's address has no default: an installation names the
//! provider it calls.
use std::{collections::BTreeSet, path::PathBuf, time::Duration};

use clap::Parser;
use secrecy::SecretString;
use url::Url;
use veoveo_mcp_artifact::contract::RetentionDays;
use veoveo_sdk_rust::{PublicDeployment, parse_allowed_host_authority};

use crate::run::{RunCompletion, RunSettings};

#[derive(Parser)]
#[command(name = "veoveo-mcp-media", about = "Media MCP server")]
pub(super) struct Args {
    #[arg(long, env = "VEOVEO_PORT", default_value_t = 8802)]
    pub port: u16,
    /// The origin where the Gateway reaches Media; Media serves MCP at
    /// `{public_base_url}/media/mcp` and registers that URL.
    #[arg(long, env = "VEOVEO_PUBLIC_BASE_URL")]
    pub public_base_url: String,
    /// The installation's public origin, under which every server lives at its own path,
    /// such as `https://veoveo.example.com`. Media derives its callback URLs from it,
    /// `{origin}/media/operations/{operation_id}/{token}`, and setting it selects webhook
    /// completion; without it, a run sends no callback URL and completes through bounded
    /// checks of the provider's status endpoint.
    #[arg(long, env = "VEOVEO_PUBLIC_ORIGIN")]
    pub public_origin: Option<Url>,
    /// The Gateway's RFC 8414 issuer; registration discovers its endpoint from there.
    #[arg(long, env = "VEOVEO_GATEWAY_ISSUER")]
    pub gateway_issuer: Url,
    /// The installation's registration secret, or the placeholder OpenShell substitutes.
    #[arg(
        long,
        env = "VEOVEO_REGISTRATION_SECRET",
        hide_env_values = true,
        value_parser = parse_secret
    )]
    pub registration_secret: SecretString,
    /// Holds Media's registration key, which keeps its namespace across restarts.
    #[arg(
        long,
        env = "VEOVEO_KEY_DIRECTORY",
        default_value = "/var/lib/veoveo/media/keys"
    )]
    pub key_directory: PathBuf,
    /// The generation provider's API origin. No default: tests and the local
    /// installation name the fake provider.
    #[arg(long, env = "MEDIA_PROVIDER_URL")]
    pub provider_url: Url,
    /// The provider's API key.
    #[arg(
        long,
        env = "MEDIA_PROVIDER_API_KEY",
        hide_env_values = true,
        value_parser = parse_secret
    )]
    pub provider_api_key: SecretString,
    /// The secret the provider signs its webhooks with.
    #[arg(
        long,
        env = "MEDIA_PROVIDER_WEBHOOK_SECRET",
        hide_env_values = true,
        value_parser = parse_secret
    )]
    pub provider_webhook_secret: SecretString,
    /// The hosts outputs may be fetched from, as `host` or `host:port`. Empty admits the
    /// provider's own host only.
    #[arg(
        long = "output-host",
        env = "MEDIA_OUTPUT_HOSTS",
        value_delimiter = ','
    )]
    pub output_hosts: Vec<String>,
    /// The most bytes one output may hold.
    #[arg(long, env = "MEDIA_OUTPUT_MAX_BYTES", default_value_t = 2 * 1024 * 1024 * 1024)]
    pub output_max_bytes: u64,
    /// How long a run waits for the provider's outcome, in seconds.
    #[arg(long, env = "MEDIA_GENERATION_TIMEOUT_SECS", default_value_t = 2 * 60 * 60)]
    pub generation_timeout_secs: u64,
    /// How long a run's Task and its prediction stay readable after it ended, in days.
    #[arg(long, env = "MEDIA_RUN_RETENTION_DAYS", default_value_t = 30)]
    pub run_retention_days: u16,
    /// How long Artifact keeps a run's outputs, in days. Unset keeps them until deleted.
    #[arg(long, env = "MEDIA_OUTPUT_RETENTION_DAYS")]
    pub output_retention_days: Option<u16>,
    #[arg(long, env = "VEOVEO_ALLOW_LOOPBACK_HOSTS", default_value_t = false)]
    pub allow_loopback_hosts: bool,
    #[arg(
        long = "allowed-host",
        env = "VEOVEO_ALLOWED_HOSTS",
        value_name = "HOST",
        value_delimiter = ',',
        value_parser = parse_allowed_host
    )]
    pub allowed_hosts: Vec<String>,
}

impl Args {
    pub fn public_deployment(&self) -> anyhow::Result<PublicDeployment> {
        PublicDeployment::new(&self.public_base_url)
    }

    pub fn run_retention(&self) -> Duration {
        Duration::from_secs(u64::from(self.run_retention_days.max(1)) * 24 * 60 * 60)
    }

    pub fn run_settings(&self) -> anyhow::Result<RunSettings> {
        let output_retention = self
            .output_retention_days
            .map(RetentionDays::new)
            .transpose()
            .map_err(|error| anyhow::anyhow!("MEDIA_OUTPUT_RETENTION_DAYS: {error}"))?;
        Ok(RunSettings {
            completion: match self.public_origin {
                Some(_) => RunCompletion::Events,
                None => RunCompletion::status_checks(),
            },
            generation_timeout: Duration::from_secs(self.generation_timeout_secs.max(60)),
            output_max_bytes: self.output_max_bytes,
            output_hosts: self
                .output_hosts
                .iter()
                .map(|host| host.trim().to_ascii_lowercase())
                .filter(|host| !host.is_empty())
                .collect::<BTreeSet<_>>(),
            output_retention,
            ..RunSettings::default()
        })
    }
}

fn parse_secret(value: &str) -> Result<SecretString, String> {
    (!value.is_empty())
        .then(|| SecretString::from(value))
        .ok_or_else(|| "secret must not be empty".to_owned())
}

fn parse_allowed_host(value: &str) -> Result<String, String> {
    let value = value.trim();
    parse_allowed_host_authority(value)
        .map(|_| value.to_owned())
        .ok_or_else(|| "expected a host authority such as host.openshell.internal:8802".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(extra: &[&str]) -> Result<Args, clap::Error> {
        let mut argv = vec![
            "veoveo-mcp-media",
            "--public-base-url",
            "http://host.openshell.internal:8802",
            "--gateway-issuer",
            "http://host.openshell.internal:8080",
            "--registration-secret",
            "placeholder",
            "--provider-api-key",
            "not-printed-key",
            "--provider-webhook-secret",
            "not-printed-secret",
        ];
        argv.extend_from_slice(extra);
        Args::try_parse_from(argv)
    }

    #[test]
    fn the_provider_address_is_required() {
        assert!(args(&[]).is_err());
        let args = args(&["--provider-url", "http://127.0.0.1:8803"]).unwrap();
        assert_eq!(args.provider_url.as_str(), "http://127.0.0.1:8803/");
        let settings = args.run_settings().unwrap();
        assert!(settings.output_hosts.is_empty());
        assert_eq!(args.port, 8802);
    }

    #[test]
    fn a_public_origin_selects_webhooks_and_its_absence_status_checks() {
        let checked = args(&["--provider-url", "http://127.0.0.1:8803"]).unwrap();
        assert_eq!(
            checked.run_settings().unwrap().completion,
            RunCompletion::status_checks()
        );
        let webhooks = args(&[
            "--provider-url",
            "http://127.0.0.1:8803",
            "--public-origin",
            "https://veoveo.example.com",
        ])
        .unwrap();
        assert_eq!(
            webhooks.run_settings().unwrap().completion,
            RunCompletion::Events
        );
    }

    #[test]
    fn output_hosts_are_normalized() {
        let args = args(&[
            "--provider-url",
            "http://127.0.0.1:8803",
            "--output-host",
            "CDN.Example, other.example:8443",
        ])
        .unwrap();
        let settings = args.run_settings().unwrap();
        assert_eq!(
            settings.output_hosts,
            BTreeSet::from(["cdn.example".to_owned(), "other.example:8443".to_owned()])
        );
    }
}
