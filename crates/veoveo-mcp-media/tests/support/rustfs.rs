//! A private RustFS server for one test process, started from the binary the dev image
//! copies out of the pinned RustFS image, with a fresh bucket for each storage handle.
//! In-process Artifact stores Media's outputs there. Requires `rustfs` on `PATH`.
#![allow(dead_code)]

use std::{
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use url::Url;
use veoveo_mcp_artifact::storage::{ObjectStorage, StorageConfig};

const ACCESS_KEY: &str = "media-test-access";
const SECRET_KEY: &str = "media-test-secret-key";

/// One RustFS process and its data directory, removed on drop.
pub struct TestRustFs {
    child: Child,
    dir: PathBuf,
    endpoint: Url,
}

impl TestRustFs {
    /// Start RustFS on a free loopback port and wait until it answers, for at most 30
    /// seconds. The port is free when chosen, but another process can take it before
    /// RustFS binds it; RustFS then exits, and the fixture starts it again on another
    /// port, at most three times.
    pub async fn start() -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = std::env::temp_dir().join(format!("media-rustfs-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(dir.join("data")).expect("RustFS data directory");
        std::fs::create_dir_all(dir.join("logs")).expect("RustFS log directory");
        let deadline = Instant::now() + Duration::from_secs(30);
        for _ in 0..3 {
            let port = TcpListener::bind("127.0.0.1:0")
                .and_then(|listener| listener.local_addr())
                .expect("a free loopback port")
                .port();
            let stderr = std::fs::File::create(dir.join("stderr.log")).expect("RustFS stderr log");
            let mut child = Command::new("rustfs")
                .arg("server")
                .arg(dir.join("data"))
                .env("RUSTFS_ADDRESS", format!("127.0.0.1:{port}"))
                .env("RUSTFS_ACCESS_KEY", ACCESS_KEY)
                .env("RUSTFS_SECRET_KEY", SECRET_KEY)
                .env("RUSTFS_OBS_LOG_DIRECTORY", dir.join("logs"))
                .env("RUSTFS_OBS_LOGGER_LEVEL", "warn")
                .stdout(Stdio::null())
                .stderr(stderr)
                .spawn()
                .expect("start rustfs; the dev image puts it on PATH");
            let endpoint = Url::parse(&format!("http://127.0.0.1:{port}")).unwrap();
            if answers(&mut child, &endpoint, &dir, deadline).await {
                return Self {
                    child,
                    dir,
                    endpoint,
                };
            }
            // RustFS exited, most likely because another process took the port.
            let _ = child.wait();
        }
        panic!(
            "RustFS exited three times without answering: {}",
            std::fs::read_to_string(dir.join("stderr.log")).unwrap_or_default()
        );
    }

    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    /// The configuration of a fresh bucket on this server.
    pub fn config(&self) -> StorageConfig {
        StorageConfig {
            internal_endpoint: self.endpoint.clone(),
            signing_endpoint: self.endpoint.clone(),
            region: "us-east-1".into(),
            bucket: format!("t{}", uuid::Uuid::now_v7().simple()),
            access_key: ACCESS_KEY.into(),
            secret_key: SECRET_KEY.into(),
            cors_origins: vec!["https://console.veoveo.test".into()],
        }
    }

    /// A prepared fresh bucket.
    pub async fn storage(&self) -> ObjectStorage {
        let storage = ObjectStorage::new(&self.config()).expect("test storage configuration");
        storage
            .prepare_within(Duration::from_secs(30))
            .await
            .expect("prepare the test bucket");
        storage
    }
}

/// True once RustFS answers at `endpoint`, false when it exits first. Panics at
/// `deadline` with RustFS's error output.
async fn answers(
    child: &mut Child,
    endpoint: &Url,
    dir: &std::path::Path,
    deadline: Instant,
) -> bool {
    let client = reqwest::Client::new();
    loop {
        if client.get(endpoint.as_str()).send().await.is_ok() {
            return true;
        }
        if child.try_wait().ok().flatten().is_some() {
            return false;
        }
        assert!(
            Instant::now() < deadline,
            "RustFS did not answer within 30 seconds: {}",
            std::fs::read_to_string(dir.join("stderr.log")).unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

impl Drop for TestRustFs {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
