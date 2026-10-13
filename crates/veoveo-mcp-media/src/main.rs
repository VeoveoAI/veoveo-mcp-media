#[tokio::main]
async fn main() -> anyhow::Result<()> {
    veoveo_mcp_media::server::run().await
}
