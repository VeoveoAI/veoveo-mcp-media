//! A private SeaweedFS for one test process, started in `weed mini` mode from the binary
//! the dev image copies out of the pinned SeaweedFS image, with a fresh bucket for each
//! storage handle. In-process Artifact stores Media's outputs there. Requires `weed` on
//! `PATH`.
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

/// SeaweedFS serves each component's gRPC at its HTTP port plus this offset, and its
/// embedded maintenance worker reaches the admin server there whatever `admin.port.grpc`
/// says, so every port the fixture picks keeps the convention.
const GRPC_PORT_OFFSET: u16 = 10_000;

/// One SeaweedFS process and its data directory, removed on drop.
pub struct TestSeaweedFs {
    child: Child,
    dir: PathBuf,
    endpoint: Url,
}

impl TestSeaweedFs {
    /// Start SeaweedFS on free loopback ports and wait until its S3 gateway answers, for
    /// at most 30 seconds. The ports are free when chosen, but another process can take
    /// one before SeaweedFS binds it; SeaweedFS then exits, and the fixture starts it
    /// again on other ports, at most three times.
    pub async fn start() -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = std::env::temp_dir().join(format!("media-seaweedfs-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(dir.join("data")).expect("SeaweedFS data directory");
        let identities = serde_json::json!({
            "identities": [{
                "name": "media",
                "credentials": [{ "accessKey": ACCESS_KEY, "secretKey": SECRET_KEY }],
                "actions": ["Admin", "Read", "Write", "List", "Tagging"],
            }]
        });
        let config = dir.join("s3.json");
        std::fs::write(&config, identities.to_string()).expect("SeaweedFS S3 identities");
        let deadline = Instant::now() + Duration::from_secs(30);
        for _ in 0..3 {
            let [master, volume, filer, s3, admin] = free_ports();
            let stderr =
                std::fs::File::create(dir.join("stderr.log")).expect("SeaweedFS stderr log");
            let port = |name: &str, port: u16| format!("-{name}={port}");
            let mut child = Command::new("weed")
                .arg("mini")
                .arg(format!("-dir={}", dir.join("data").display()))
                .arg("-ip=127.0.0.1")
                .arg("-ip.bind=127.0.0.1")
                .arg(port("master.port", master))
                .arg(port("master.port.grpc", master + GRPC_PORT_OFFSET))
                .arg(port("volume.port", volume))
                .arg(port("volume.port.grpc", volume + GRPC_PORT_OFFSET))
                .arg(port("filer.port", filer))
                .arg(port("filer.port.grpc", filer + GRPC_PORT_OFFSET))
                .arg(port("s3.port", s3))
                .arg(port("s3.port.grpc", s3 + GRPC_PORT_OFFSET))
                .arg(port("admin.port", admin))
                .arg(port("admin.port.grpc", admin + GRPC_PORT_OFFSET))
                .arg("-admin.ui=false")
                .arg("-webdav=false")
                .arg("-s3.port.iceberg=0")
                .arg("-s3.port.lance=0")
                .arg(format!("-s3.config={}", config.display()))
                .stdout(Stdio::null())
                .stderr(stderr)
                .spawn()
                .expect("start weed; the dev image puts it on PATH");
            let endpoint = Url::parse(&format!("http://127.0.0.1:{s3}")).unwrap();
            if answers(&mut child, &endpoint, &dir, deadline).await {
                return Self {
                    child,
                    dir,
                    endpoint,
                };
            }
            // SeaweedFS exited, most likely because another process took a port.
            let _ = child.wait();
        }
        panic!(
            "SeaweedFS exited three times without answering: {}",
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

/// Five loopback ports, each free together with its gRPC port at the offset above.
fn free_ports() -> [u16; 5] {
    let mut ports = [0u16; 5];
    let mut held = Vec::new();
    let mut found = 0;
    while found < ports.len() {
        let Ok(listener) = TcpListener::bind("127.0.0.1:0") else {
            continue;
        };
        let port = listener.local_addr().expect("a bound loopback port").port();
        let Some(grpc) = port.checked_add(GRPC_PORT_OFFSET) else {
            continue;
        };
        let Ok(grpc_listener) = TcpListener::bind(("127.0.0.1", grpc)) else {
            continue;
        };
        if ports[..found]
            .iter()
            .any(|taken| *taken == port || *taken == grpc || *taken + GRPC_PORT_OFFSET == port)
        {
            continue;
        }
        ports[found] = port;
        found += 1;
        // Hold every chosen port until all are chosen, so no two picks repeat.
        held.push((listener, grpc_listener));
    }
    ports
}

/// True once SeaweedFS's S3 gateway answers at `endpoint`, false when the process exits
/// first. Panics at `deadline` with SeaweedFS's error output.
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
            "SeaweedFS did not answer within 30 seconds: {}",
            std::fs::read_to_string(dir.join("stderr.log")).unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

impl Drop for TestSeaweedFs {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
