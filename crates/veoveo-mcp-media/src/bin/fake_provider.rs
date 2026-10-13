//! The fake generation provider for tests and the local installation. It listens on a
//! loopback address and posts every outcome to the submission's webhook, signed with
//! `MEDIA_PROVIDER_WEBHOOK_SECRET`. It never reaches the real provider.
use std::{net::SocketAddr, time::Duration};

use clap::Parser;
use veoveo_mcp_media::fake::{FakeProvider, FakeProviderConfig};

#[derive(Parser)]
#[command(
    name = "veoveo-mcp-media-fake-provider",
    about = "Fake generation provider"
)]
struct Args {
    #[arg(
        long,
        env = "MEDIA_FAKE_PROVIDER_LISTEN",
        default_value = "127.0.0.1:8803"
    )]
    listen: SocketAddr,
    #[arg(long, env = "MEDIA_PROVIDER_WEBHOOK_SECRET", hide_env_values = true)]
    webhook_secret: String,
    /// How long a prediction runs before its outcome is posted, in milliseconds.
    #[arg(long, env = "MEDIA_FAKE_PROVIDER_DELAY_MS", default_value_t = 500)]
    delay_ms: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args = Args::parse();
    let fake = FakeProvider::start(FakeProviderConfig {
        listen: args.listen,
        webhook_secret: args.webhook_secret,
        delay: Duration::from_millis(args.delay_ms),
        deliver: true,
    })
    .await?;
    println!("the fake provider listens at {}", fake.base());
    tokio::signal::ctrl_c().await?;
    Ok(())
}
