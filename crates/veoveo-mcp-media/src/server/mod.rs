//! The hosted process: read the model catalog, register with the Gateway, connect with
//! the registration's database login, migrate, declare Media's usage kinds, then serve MCP
//! and the provider's callback route while the run worker, the catalog refresh, the change
//! watcher and the sweep run. Stopping deletes the registration.
mod config;
mod registration;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use chrono::Utc;
use clap::Parser;
use tokio_util::sync::CancellationToken;
use veoveo_sdk_rust::{
    TelemetryGuard,
    durable_tasks::{CallbackUrls, DurableTasks, TaskWorker},
    hosting::{Hosted, HostedServer, ServerCalls, operations::operation_callbacks},
    init_server_telemetry,
    subscriptions::SubscriptionHub,
    usage::UsageLedger,
};
use veoveo_task_runtime::{TaskRuntime, TaskRuntimeConfig};

use crate::{
    catalog::Catalog,
    mcp::{self, MediaApp, MediaMcp, MediaSubscriptions, MediaTasks, watch_changes},
    provider::{ProviderClient, ProviderWebhooks},
    run::{self, MediaUsage, MediaWork},
    store::MediaStore,
};

use config::Args;
use registration::Registered;

/// How often the sweep looks for ended runs past their retention.
const SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);

pub async fn run() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let _ = dotenvy::dotenv();
    let _telemetry: TelemetryGuard =
        init_server_telemetry("veoveo-mcp-media", "info,veoveo_mcp_media=debug")?;
    let args = Args::parse();
    let deployment = args.public_deployment()?;
    let declaration = mcp::registration(&deployment)?;
    mcp::REGISTRATION
        .set(declaration.clone())
        .map_err(|_| anyhow::anyhow!("Media's registration was declared twice"))?;
    mcp::force_setup();
    let http = reqwest::Client::builder()
        .build()
        .context("building the provider HTTP client")?;
    let provider = ProviderClient::new(
        http.clone(),
        args.provider_url.clone(),
        args.provider_api_key.clone(),
    )
    .context("configuring the provider")?;
    let registered = registration::register(&args, declaration).await?;
    // The registration lasts while Media's login holds a live process instance. Once
    // serving ends, on SIGTERM or Ctrl-C or on a failure, the client deletes it, which
    // also drops the login.
    let Registered {
        client,
        registration,
        metadata,
        key,
        http: gateway_http,
    } = registered;
    let issued = registration.clone();
    let mut served = None;
    let maintained = client
        .maintain(registration, async {
            served = Some(
                serve(
                    &args,
                    &deployment,
                    &issued,
                    Gateway {
                        metadata: &metadata,
                        key: &key,
                        http: gateway_http,
                    },
                    provider,
                    http,
                )
                .await,
            );
        })
        .await;
    if let Err(error) = maintained {
        tracing::warn!(%error, "Media could not remove its registration");
    }
    served.context("Media's registration ended before serving finished")?
}

/// What calls through the Gateway need from the registration.
struct Gateway<'a> {
    metadata: &'a veoveo_sdk_rust::registration::ClientMetadata,
    key: &'a veoveo_sdk_rust::registration::statement::ServerKeyPair,
    http: reqwest::Client,
}

/// Connect with the registration's database login, migrate Media's schema and serve
/// until SIGTERM or Ctrl-C, or until the process instance loses its lock.
async fn serve(
    args: &Args,
    deployment: &veoveo_sdk_rust::PublicDeployment,
    registration: &veoveo_sdk_rust::registration::client::Registration,
    gateway: Gateway<'_>,
    provider: ProviderClient,
    http: reqwest::Client,
) -> Result<()> {
    let grant = registration
        .database()
        .context("the Gateway issued no database login for Media")?;
    let runtime = TaskRuntime::connect(
        TaskRuntimeConfig::new(
            grant.url.as_str(),
            grant.username.as_str(),
            grant.password.clone(),
        )?,
        crate::server(),
    )
    .await
    .context("connecting to the installation database")?;
    let platform = runtime.platform_store().clone();
    platform
        .migrate_server(&crate::server(), crate::migrator())
        .await
        .context("applying Media's migrations")?;
    let usage = UsageLedger::new(&runtime);
    usage
        .declare::<MediaUsage>()
        .await
        .context("declaring Media's usage kinds")?;
    let instance = runtime.instance().clone();
    let store = MediaStore::new(platform.clone());
    let catalog = Arc::new(Catalog::new(provider.clone()));
    let calls = ServerCalls::discover(
        registration,
        gateway.metadata,
        gateway.key,
        gateway.http.clone(),
    )
    .await
    .context("discovering the Gateway's token endpoint")?;
    let settings = Arc::new(args.run_settings(deployment)?);
    let work = MediaWork::new(store.clone(), catalog.clone(), provider, http, settings)?;
    let mut worker = TaskWorker::new(runtime.clone()).calls(calls).usage(usage);
    // The installation's public origin selects webhook completion, with callback URLs
    // under Media's path on it; without one, runs send no callback URL and check the
    // provider's status endpoint.
    match deployment.origin() {
        Some(origin) => {
            tracing::info!("runs complete through the provider's webhooks");
            worker = worker.callbacks(
                CallbackUrls::new(origin.url().clone(), crate::server()).map_err(|_| {
                    anyhow::anyhow!("the public origin cannot carry Media's callback paths")
                })?,
            );
        }
        None => tracing::info!("runs complete through bounded status checks"),
    }
    let hub = Arc::new(SubscriptionHub::new());
    let app = Arc::new(MediaApp {
        store: store.clone(),
        catalog: catalog.clone(),
    });
    let tasks = MediaTasks::new(
        runtime.clone(),
        store.clone(),
        catalog.clone(),
        args.run_retention(),
    );
    let subscriptions = MediaSubscriptions::new(store.clone(), hub.clone());

    let stop = CancellationToken::new();
    let refreshing = tokio::spawn(catalog.clone().keep_fresh(stop.clone()));
    let watching = tokio::spawn(watch_changes(store.clone(), hub));
    let sweeping = tokio::spawn(sweep(store.clone(), args.run_retention(), stop.clone()));
    let mut working = tokio::spawn(run::work(worker, work, stop.clone()));

    let hosted = HostedServer::for_domain::<MediaMcp>()
        .deployment(deployment, args.allow_loopback_hosts)?
        .allowed_hosts(args.allowed_hosts.iter().cloned())
        .internal_trust(registration.access_tokens(), gateway.http)?
        .handler(move || {
            Hosted::new(MediaMcp::new(app.clone())).with_tasks(DurableTasks::with_resources(
                tasks.clone(),
                subscriptions.clone(),
            ))
        })
        // The provider posts its outcomes without Gateway authentication; the route
        // admits only events whose signature and callback token check out.
        .public_routes(operation_callbacks(
            runtime.clone(),
            ProviderWebhooks::new(args.provider_webhook_secret.clone()),
        ))
        .build();
    // Serving ends on SIGTERM or Ctrl-C, or when Media can no longer claim its Tasks,
    // which includes the loss of its instance lock.
    let served = tokio::select! {
        served = hosted.serve(SocketAddr::from(([0, 0, 0, 0], args.port))) => served,
        worked = &mut working => match worked {
            Ok(Ok(())) => Err(anyhow::anyhow!("Media's run worker stopped")),
            Ok(Err(error)) => Err(anyhow::Error::from(error).context("Media stopped claiming runs")),
            Err(error) => Err(anyhow::Error::from(error).context("Media's run worker failed")),
        },
    };
    // The worker drains its running Tasks; a resumable run it cannot finish is left for
    // another instance's recovery.
    stop.cancel();
    if !working.is_finished() {
        let _ = working.await;
    }
    watching.abort();
    let _ = refreshing.await;
    let _ = sweeping.await;
    // Deleting the instance's row releases the Tasks it still owns at once.
    if let Err(error) = instance.shutdown().await {
        tracing::warn!(%error, "Media's process instance did not stop cleanly");
    }
    platform.close().await;
    served
}

/// Delete ended runs past their retention, in batches, until `stop`.
async fn sweep(store: MediaStore, retention: Duration, stop: CancellationToken) {
    let retention = chrono::TimeDelta::from_std(retention).unwrap_or(chrono::TimeDelta::MAX);
    loop {
        let cutoff = Utc::now() - retention;
        loop {
            match store.prune(cutoff).await {
                Ok(0) => break,
                Ok(_) => continue,
                Err(error) => {
                    tracing::warn!(%error, "the Media sweep could not prune ended runs");
                    break;
                }
            }
        }
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(SWEEP_INTERVAL) => {}
        }
    }
}
