//! Shared fixtures: a fresh database migrated for Artifact and Media under their own
//! logins, a private RustFS, Artifact in process, the fake provider, and both servers on
//! the SDK's in-process mesh with Media's run worker on its own process instance.
//! Requires the PostgreSQL 18 server programs and `rustfs` on `PATH`.
#![allow(dead_code)]

pub mod rustfs;

use std::{collections::BTreeSet, future::Future, net::SocketAddr, sync::Arc, time::Duration};

use secrecy::SecretString;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use veoveo_mcp_artifact::{
    contract::ArtifactPermission,
    mcp::{
        AUTHORIZATION as ARTIFACT_AUTHORIZATION, ArtifactApp, ArtifactMcp, ArtifactSubscriptions,
        TenantHubs, watch_changes as watch_artifact,
    },
    service::{ArtifactService, run_verifications},
    share::ShareBase,
    store::ArtifactStore,
};
use veoveo_mcp_media::{
    catalog::Catalog,
    contract::{MediaPermission, MediaPredictionId},
    fake::{FakeProvider, FakeProviderConfig},
    mcp::{AUTHORIZATION, MediaApp, MediaMcp, MediaSubscriptions, MediaTasks, watch_changes},
    provider::{ProviderClient, ProviderWebhooks},
    run::{self, MediaUsage, MediaWork, RunCompletion, RunSettings},
    store::MediaStore,
};
use veoveo_platform_store::PlatformStore;
use veoveo_sdk_rust::{
    durable_tasks::{CallbackUrls, DurableTasks, TaskWorker},
    hosting::{
        Hosted, ListenOnly, ServerCalls,
        operations::operation_callbacks,
        testing::{self, RunningMesh, TestMesh},
    },
    subscriptions::SubscriptionHub,
    usage::{TASK_USAGE_META_KEY, UsageLedger},
};
use veoveo_task_runtime::{Instance, TaskRuntime, TaskSnapshot, TaskStatus};
use veoveo_test_fixtures::store::TestDb;
use veoveo_types::{ServerSlug, TaskId};

use rustfs::TestRustFs;

/// The secret the fake provider signs its webhooks with.
pub const WEBHOOK_SECRET: &str = "whsec_media-test-secret";
/// How long a test waits for one state.
pub const DEADLINE: Duration = Duration::from_secs(60);

pub fn media() -> ServerSlug {
    veoveo_mcp_media::server()
}

pub fn artifact() -> ServerSlug {
    veoveo_mcp_artifact::server()
}

/// Run one test case within 180 seconds.
pub async fn within<T>(case: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(180), case)
        .await
        .expect("the test case exceeded 180 seconds")
}

/// Media's run worker on its own process instance.
pub struct Worker {
    pub runtime: TaskRuntime,
    stop: CancellationToken,
    join: tokio::task::JoinHandle<()>,
}

impl Worker {
    /// Stop claiming, drain and wait for the worker to end, as SIGTERM does.
    pub async fn stop(self) {
        self.stop.cancel();
        let _ = self.join.await;
    }
}

/// Artifact and Media on one mesh over one database, with the fake provider.
pub struct Harness {
    pub db: TestDb,
    pub mesh: RunningMesh,
    fake: Option<FakeProvider>,
    pub store: MediaStore,
    pub media_calls: ServerCalls,
    pub callbacks: CallbackUrls,
    pub catalog: Arc<Catalog>,
    pub worker: Option<Worker>,
    work: MediaWork,
    /// Whether the worker sends callback URLs.
    events: bool,
    /// Media's HTTP process's own runtime, which stores provider events.
    pub routes: TaskRuntime,
    _rustfs: TestRustFs,
    background: Vec<tokio::task::JoinHandle<()>>,
    stop: CancellationToken,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.stop.cancel();
        for task in &self.background {
            task.abort();
        }
    }
}

/// What a harness grants the test user: every Media and Artifact permission by default.
pub fn all_permissions() -> Vec<veoveo_types::PermissionName> {
    MediaPermission::ALL
        .iter()
        .map(|permission| permission.name())
        .chain(
            [
                ArtifactPermission::ArtifactsRead,
                ArtifactPermission::ArtifactsWrite,
            ]
            .into_iter()
            .map(|permission| permission.name()),
        )
        .collect()
}

/// A harness whose runs complete through the fake's webhooks.
pub async fn harness(deliver: bool) -> Harness {
    harness_with(deliver, Completion::Events, all_permissions()).await
}

pub async fn harness_granting(
    deliver: bool,
    granted: Vec<veoveo_types::PermissionName>,
) -> Harness {
    harness_with(deliver, Completion::Events, granted).await
}

/// How a harness's runs learn their outcome.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    /// The worker knows the public origin, so runs send a webhook URL.
    Events,
    /// The worker knows no public origin, so runs check the status endpoint.
    Checks,
}

pub async fn harness_with(
    deliver: bool,
    completion: Completion,
    granted: Vec<veoveo_types::PermissionName>,
) -> Harness {
    let fake = FakeProvider::start(FakeProviderConfig {
        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        webhook_secret: WEBHOOK_SECRET.into(),
        delay: Duration::from_millis(100),
        deliver,
    })
    .await
    .unwrap();
    let provider = Provider {
        url: fake.base().clone(),
        api_key: SecretString::from("media-test-api-key"),
        webhook_secret: SecretString::from(WEBHOOK_SECRET),
        fake: Some(fake),
    };
    let completion = match completion {
        Completion::Events => RunCompletion::Events,
        Completion::Checks => RunCompletion::Checks(
            veoveo_task_runtime::CheckSchedule::new(
                Duration::from_millis(200),
                Duration::from_millis(200),
                1.2,
                60,
            )
            .unwrap(),
        ),
    };
    let settings = RunSettings {
        completion,
        generation_timeout: Duration::from_secs(120),
        billing_schedule: veoveo_task_runtime::CheckSchedule::new(
            Duration::from_millis(100),
            Duration::from_millis(200),
            1.5,
            10,
        )
        .unwrap(),
        billing_timeout: Duration::from_secs(60),
        output_max_bytes: 64 * 1024 * 1024,
        output_timeout: Duration::from_secs(60),
        output_hosts: BTreeSet::new(),
        verification_timeout: Duration::from_secs(60),
        output_retention: None,
    };
    harness_for(provider, settings, None, granted).await
}

/// The provider a harness's runs reach: the fake, or a real one in the qualification.
pub struct Provider {
    pub url: url::Url,
    pub api_key: SecretString,
    pub webhook_secret: SecretString,
    pub fake: Option<FakeProvider>,
}

/// A harness over `provider` with `settings`. Under events completion the worker's
/// callback URLs lie under `public_origin`, or under the mesh when it is `None`.
pub async fn harness_for(
    provider: Provider,
    settings: RunSettings,
    public_origin: Option<url::Url>,
    granted: Vec<veoveo_types::PermissionName>,
) -> Harness {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let db = TestDb::new().await;
    db.migrate_server(&artifact(), veoveo_mcp_artifact::migrator())
        .await;
    db.migrate_server(&media(), veoveo_mcp_media::migrator())
        .await;
    let stop = CancellationToken::new();
    let mut background = Vec::new();

    // Artifact in process, over its own login and a private RustFS.
    let rustfs = TestRustFs::start().await;
    let storage = rustfs.storage().await;
    let artifact_platform = db.server_store(&artifact()).await;
    let registry = Arc::new(veoveo_mcp_artifact::contract::audit_registry().unwrap());
    veoveo_audit::AuditStore::new(artifact_platform.clone(), registry.clone())
        .declare_as_server(&artifact())
        .await
        .unwrap();
    let artifact_tasks = TaskRuntime::new(
        artifact_platform.clone(),
        artifact(),
        Arc::new(
            Instance::start(&artifact_platform, &artifact())
                .await
                .unwrap(),
        ),
    );
    let service = Arc::new(ArtifactService::new(
        ArtifactStore::new(artifact_platform.clone(), registry),
        storage,
        artifact_tasks,
        ShareBase::parse("http://artifact.test:8801").unwrap(),
    ));
    background.push(tokio::spawn({
        let service = service.clone();
        let stop = stop.clone();
        async move {
            let _ = run_verifications(service, stop).await;
        }
    }));
    let hubs = Arc::new(TenantHubs::default());
    background.push(tokio::spawn(watch_artifact(
        artifact_platform.clone(),
        hubs.clone(),
    )));
    let artifact_app = Arc::new(ArtifactApp {
        service: (*service).clone(),
        hubs,
    });

    // Media's catalog of the provider's registry.
    let http = reqwest::Client::new();
    let Provider {
        url: provider_url,
        api_key,
        webhook_secret,
        fake,
    } = provider;
    let provider = ProviderClient::new(http.clone(), provider_url, api_key).unwrap();
    let catalog = Arc::new(Catalog::new(provider.clone()));
    catalog.refresh().await.unwrap();

    // Media's HTTP process: its runtime stores provider events and serves Tasks.
    let media_platform = db.server_store(&media()).await;
    let routes = TaskRuntime::new(
        media_platform.clone(),
        media(),
        Arc::new(Instance::start(&media_platform, &media()).await.unwrap()),
    );
    UsageLedger::new(&routes)
        .declare::<MediaUsage>()
        .await
        .unwrap();
    let store = MediaStore::new(media_platform.clone());
    let hub = Arc::new(SubscriptionHub::new());
    background.push(tokio::spawn(watch_changes(store.clone(), hub.clone())));

    let mut mesh = TestMesh::bind().await;
    mesh.register((*ARTIFACT_AUTHORIZATION).clone());
    mesh.serve(
        testing::for_domain::<ArtifactMcp>()
            .handler(move || {
                Hosted::new(ArtifactMcp::new(artifact_app.clone())).with_tasks(ListenOnly::new(
                    ArtifactSubscriptions::new(artifact_app.clone()),
                ))
            })
            .build(),
    );
    let media_calls = mesh.register((*AUTHORIZATION).clone());
    let callbacks = CallbackUrls::new(
        public_origin.unwrap_or_else(|| mesh.base().clone()),
        media(),
    )
    .unwrap();
    let app = Arc::new(MediaApp {
        store: store.clone(),
        catalog: catalog.clone(),
    });
    let tasks = MediaTasks::new(
        routes.clone(),
        store.clone(),
        catalog.clone(),
        Duration::from_secs(24 * 60 * 60),
    );
    let subscriptions = MediaSubscriptions::new(store.clone(), hub);
    mesh.serve(
        testing::for_domain::<MediaMcp>()
            .public_routes(operation_callbacks(
                routes.clone(),
                ProviderWebhooks::new(webhook_secret),
            ))
            .handler(move || {
                Hosted::new(MediaMcp::new(app.clone())).with_tasks(DurableTasks::with_resources(
                    tasks.clone(),
                    subscriptions.clone(),
                ))
            })
            .build(),
    );
    let mesh = mesh.granting(granted).start();
    let events = settings.completion == RunCompletion::Events;
    let work = MediaWork::new(
        store.clone(),
        catalog.clone(),
        provider,
        http,
        Arc::new(settings),
    )
    .unwrap();
    let mut harness = Harness {
        db,
        mesh,
        fake,
        store,
        media_calls,
        callbacks,
        catalog,
        worker: None,
        work,
        events,
        routes,
        _rustfs: rustfs,
        background,
        stop,
    };
    harness.start_worker().await;
    harness
}

impl Harness {
    /// Start a run worker on a new process instance.
    pub async fn start_worker(&mut self) {
        let platform: PlatformStore = self.db.server_store(&media()).await;
        let instance = Arc::new(Instance::start(&platform, &media()).await.unwrap());
        let runtime = TaskRuntime::new(platform, media(), instance);
        let mut worker = TaskWorker::new(runtime.clone())
            .calls(self.media_calls.clone())
            .usage(UsageLedger::new(&runtime))
            .drain(Duration::from_secs(5));
        if self.events {
            worker = worker.callbacks(self.callbacks.clone());
        }
        let stop = CancellationToken::new();
        let join = tokio::spawn({
            let work = self.work.clone();
            let stop = stop.clone();
            async move {
                let _ = run::work(worker, work, stop).await;
            }
        });
        self.worker = Some(Worker {
            runtime,
            stop,
            join,
        });
    }

    /// Stop the run worker, as its process does on SIGTERM.
    pub async fn stop_worker(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.stop().await;
        }
    }

    /// Restart the fake provider on its address, as the installation restarts its
    /// process: the new fake holds none of the old one's predictions.
    pub async fn restart_fake(&mut self) {
        let fake = self
            .fake
            .take()
            .expect("the harness runs the fake provider");
        self.fake = Some(fake.restarted().await.unwrap());
    }

    /// The provider client Media's runs call.
    pub fn provider(&self) -> &ProviderClient {
        &self.work.provider
    }

    /// The fake provider of a harness that runs one.
    pub fn fake(&self) -> &FakeProvider {
        self.fake
            .as_ref()
            .expect("the harness runs the fake provider")
    }

    pub fn runtime(&self) -> &TaskRuntime {
        &self.routes
    }

    /// Media's Task service, as the hosted server answers `tasks/*` with it.
    pub fn tasks(&self) -> MediaTasks {
        MediaTasks::new(
            self.routes.clone(),
            self.store.clone(),
            self.catalog.clone(),
            Duration::from_secs(3600),
        )
    }

    /// Start a run of `model` as the test user and return its Task.
    pub async fn run(&self, model: &str, input: Value) -> String {
        let started = self
            .mesh
            .start_task("media", "run", json!({"model": model, "input": input}))
            .await;
        started["result"]["taskId"]
            .as_str()
            .unwrap_or_else(|| panic!("no Task started: {started}"))
            .to_owned()
    }

    pub async fn task(&self, task: &str) -> Value {
        self.mesh.task("media", task).await["result"].clone()
    }

    /// Wait until the Task's MCP view satisfies `done`.
    pub async fn until(&self, task: &str, done: impl Fn(&Value) -> bool) -> Value {
        tokio::time::timeout(DEADLINE, async {
            loop {
                let view = self.task(task).await;
                if done(&view) {
                    return view;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("Task {task} never reached the expected state"))
    }

    /// Wait until the stored Task satisfies `done`, reaping released instances.
    pub async fn stored_until(
        &self,
        task: &str,
        done: impl Fn(&TaskSnapshot) -> bool,
    ) -> TaskSnapshot {
        let id = TaskId::parse(task).unwrap();
        tokio::time::timeout(DEADLINE, async {
            loop {
                if let Some(snapshot) = self.routes.get(id).await.unwrap()
                    && done(&snapshot)
                {
                    return snapshot;
                }
                self.db.a.reap_instances().await.unwrap();
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("Task {task} never reached the expected stored state"))
    }

    /// Wait until the run parks on its submitted generation, and return its prediction.
    pub async fn parked(&self, task: &str) -> MediaPredictionId {
        self.stored_until(task, |snapshot| {
            snapshot.status == TaskStatus::Waiting && snapshot.owner_instance.is_none()
        })
        .await;
        let predictions = self.fake().predictions();
        predictions
            .last()
            .cloned()
            .expect("the fake accepted the run")
    }

    /// Download an artifact as the test user through Artifact's tools.
    pub async fn download(&self, artifact_uri: &str) -> Vec<u8> {
        let artifact_id = artifact_uri
            .strip_prefix("artifact://artifacts/")
            .expect("an artifact address");
        let answer = self
            .mesh
            .call(
                "artifact",
                "create_download",
                json!({"artifactId": artifact_id}),
            )
            .await;
        let transfer = &answer["result"]["structuredContent"]["get"];
        let url = transfer["url"]
            .as_str()
            .unwrap_or_else(|| panic!("no download: {answer}"));
        let response = reqwest::Client::new().get(url).send().await.unwrap();
        assert!(response.status().is_success(), "{}", response.status());
        response.bytes().await.unwrap().to_vec()
    }
}

/// The usage records a Task view carries.
pub fn usage_of(view: &Value) -> Vec<Value> {
    view["_meta"][TASK_USAGE_META_KEY]
        .as_array()
        .cloned()
        .unwrap_or_default()
}
