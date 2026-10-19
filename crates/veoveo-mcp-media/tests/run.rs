//! Generations end to end on the SDK's in-process mesh: Media runs against the fake
//! provider over a fresh database, parks on the provider's operation, takes the signed
//! webhook on its public route, streams the output into Artifact in process over a
//! private SeaweedFS for the run's caller, and records the run's estimated and actual cost.
mod support;

use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use axum::http::StatusCode;
use chrono::{TimeDelta, Utc};
use serde_json::{Value, json};
use support::{
    Completion, Harness, all_permissions, harness, harness_granting, harness_with, usage_of, within,
};
use veoveo_hub_sdk::{
    AccessTokenSubject, ForwardedRequest, HubInternalTokenIssuer, HubProfileId, HubRequestContext,
    OAuthClientId, ProtectedResourceId, TokenIssuer,
    definition::{CollectionRequest, NoFilters},
    durable_tasks::DurableTaskService,
    hosting::testing::{self, MESH_PROFILE},
    registration::ResourceIndicator,
};
use veoveo_mcp_artifact::contract::ArtifactPermission;
use veoveo_mcp_media::{
    contract::{
        GenerationResult, MediaPermission, MediaPredictionId, PredictionCollection,
        PredictionStatus,
    },
    fake::{FAILING_MODEL, IMAGE_BYTES, IMAGE_MODEL, LARGE_BYTES, LARGE_MODEL, output_byte},
    mcp::identity,
    predictions::Prediction,
    provider::generation_key,
};
use veoveo_task::{OperationState, TaskStatus};
use veoveo_types::{InvocationMode, PageSize, TaskId};

fn prompt() -> Value {
    json!({"prompt": "a lighthouse at dusk"})
}

fn expected(length: u64) -> Vec<u8> {
    (0..length).map(output_byte).collect()
}

/// The test user as the owner of their Tasks, as Media reads their predictions.
fn owner() -> veoveo_task::TaskOwner {
    identity::task_owner(&forwarded_user()).unwrap()
}

/// The test user's prediction of `task`, through core's Task runtime and Media's row.
async fn prediction_of(harness: &Harness, task: &str) -> Prediction {
    harness
        .predictions()
        .by_task(&owner(), TaskId::parse(task).unwrap())
        .await
        .unwrap()
        .expect("the test user's run")
}

async fn status_of(harness: &Harness, task: &str) -> PredictionStatus {
    prediction_of(harness, task).await.status()
}

#[tokio::test]
async fn a_run_stores_its_output_in_artifact_and_records_its_cost() {
    within(async {
        let harness = harness(true).await;
        let task = harness.run(IMAGE_MODEL, prompt()).await;
        let completed = harness
            .until(&task, |view| view["status"] == "completed")
            .await;
        let product = &completed["result"]["structuredContent"];
        let result = &product["metadata"];
        assert_eq!(
            result["format"],
            "https://veoveo.ai/formats/media-generation"
        );
        assert_eq!(result["taskId"], task);
        assert_eq!(result["prediction"]["completion"], "events");
        let prediction = result["prediction"]["predictionId"].as_str().unwrap();
        assert_eq!(
            product["resultUri"],
            format!("media://prediction/{prediction}/result")
        );
        assert_eq!(
            completed["result"]["content"][0]["uri"],
            product["resultUri"]
        );
        let text: Value =
            serde_json::from_str(completed["result"]["content"][1]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(&text, product);
        let artifacts = result["artifacts"].as_array().unwrap();
        assert_eq!(artifacts.len(), 1, "{result}");
        assert_eq!(artifacts[0]["mediaType"], "image/png");
        assert_eq!(artifacts[0]["size"], IMAGE_BYTES);
        let artifact_uri = artifacts[0]["artifactUri"].as_str().unwrap();
        assert!(
            !result.to_string().contains("X-Amz-Signature"),
            "no download URL"
        );

        // The caller reads the output through Artifact, and its bytes are the provider's.
        assert_eq!(harness.download(artifact_uri).await, expected(IMAGE_BYTES));

        // The estimate is reserved before dispatch and the reconciled bill replaces it.
        let usage = usage_of(&completed);
        assert_eq!(usage.len(), 1, "{completed}");
        assert_eq!(usage[0]["kind"], "media.generation");
        assert_eq!(usage[0]["phase"], "actual");
        assert_eq!(usage[0]["unit"], "Requests");
        assert_eq!(usage[0]["money"]["currency"], "USD");
        assert_eq!(usage[0]["money"]["amount"], "0.04");

        // The prediction is readable, and listed, for its owner.
        assert_eq!(
            status_of(&harness, &task).await,
            PredictionStatus::Succeeded
        );
        let page = harness
            .predictions()
            .page(
                &owner(),
                &CollectionRequest::<PredictionCollection>::new(
                    NoFilters {},
                    PageSize::new(100).unwrap(),
                ),
            )
            .await
            .unwrap();
        assert_eq!(page.items().len(), 1);
        assert_eq!(page.items()[0].prediction.output_count as usize, 1);
        assert_eq!(
            page.items()[0].prediction.status,
            PredictionStatus::Succeeded
        );
        // A result snapshot is stamped before the Task transition commits. The prediction
        // projects core's lifecycle timestamp; every other summary field must agree.
        let generated: GenerationResult = serde_json::from_value(result.clone()).unwrap();
        let persisted = page.items()[0].prediction.clone();
        assert!(generated.prediction.completed_at.is_some());
        assert!(persisted.completed_at.is_some());
        let mut snapshot = generated.prediction;
        snapshot.completed_at = persisted.completed_at;
        assert_eq!(snapshot, persisted);
        assert_eq!(harness.fake().submissions(), 1);
    })
    .await;
}

#[tokio::test]
async fn an_input_the_schema_refuses_starts_no_task() {
    within(async {
        let harness = harness(true).await;
        for (model, input) in [
            (IMAGE_MODEL, json!({})),
            (IMAGE_MODEL, json!({"prompt": "x", "extra": true})),
            ("fake/unknown", prompt()),
        ] {
            let answer = harness
                .mesh
                .start_task("media", "run", json!({"model": model, "input": input}))
                .await;
            assert!(answer["error"].is_object(), "{answer}");
            assert!(answer["result"]["taskId"].is_null(), "{answer}");
        }
        assert_eq!(harness.fake().submissions(), 0);
    })
    .await;
}

#[tokio::test]
async fn a_signed_webhook_settles_once_and_forgeries_and_repeats_change_nothing() {
    within(async {
        let harness = harness(false).await;
        let task = harness.run(IMAGE_MODEL, prompt()).await;
        let prediction = harness.parked(&task).await;
        let parked = harness.task(&task).await;
        assert_eq!(parked["status"], "working", "{parked}");
        let estimate = usage_of(&parked);
        assert_eq!(estimate.len(), 1, "{parked}");
        assert_eq!(estimate[0]["phase"], "estimate");
        // The estimate is the provider's quote for this input, above the model's base
        // price.
        assert_eq!(estimate[0]["money"]["amount"], "0.04");
        assert_eq!(
            status_of(&harness, &task).await,
            PredictionStatus::Processing
        );

        // A forged signature is refused before anything is stored.
        assert_eq!(
            harness
                .fake()
                .post_event(&prediction, "evt_forged", false)
                .await
                .unwrap(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(harness.task(&task).await["status"], "working");

        // The signed outcome settles the run, and its repeat, which carries the
        // prediction's identity as the provider's first event does, changes nothing.
        assert_eq!(
            harness.fake().complete(&prediction).await.unwrap(),
            StatusCode::NO_CONTENT
        );
        let completed = harness
            .until(&task, |view| view["status"] == "completed")
            .await;
        assert_eq!(
            harness
                .fake()
                .post_event(&prediction, prediction.as_str(), true)
                .await
                .unwrap(),
            StatusCode::NO_CONTENT
        );
        // A later final event is stored as late and leaves the outcome alone.
        assert_eq!(
            harness.fake().complete(&prediction).await.unwrap(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(harness.task(&task).await["result"], completed["result"]);
        assert_eq!(harness.fake().submissions(), 1);
    })
    .await;
}

#[tokio::test]
async fn a_run_parked_across_a_restart_resumes_on_another_worker_without_resubmitting() {
    within(async {
        let mut harness = harness(false).await;
        let task = harness.run(IMAGE_MODEL, prompt()).await;
        let prediction = harness.parked(&task).await;
        // The process stops while the run waits; a parked Task holds no worker.
        harness.stop_worker().await;
        let stored = harness.stored_until(&task, |_| true).await;
        assert_eq!(stored.status, TaskStatus::Waiting);
        assert_eq!(stored.owner_instance, None);
        harness.db.a.reap_instances().await.unwrap();

        // The outcome reaches the HTTP process while no worker runs, and a new worker
        // finishes the run from where it parked.
        assert_eq!(
            harness.fake().complete(&prediction).await.unwrap(),
            StatusCode::NO_CONTENT
        );
        harness.start_worker().await;
        let completed = harness
            .until(&task, |view| view["status"] == "completed")
            .await;
        assert_eq!(
            completed["result"]["structuredContent"]["metadata"]["artifacts"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(harness.fake().submissions(), 1);
    })
    .await;
}

#[tokio::test]
async fn cancellation_while_a_quote_is_held_never_dispatches_a_generation() {
    within(async {
        let harness = harness(false).await;
        harness.fake().hold_quotes(true);
        let task = harness.run(IMAGE_MODEL, prompt()).await;
        harness.fake().quote_is_held().await;
        harness
            .tasks()
            .cancel_task(&forwarded_user(), task.clone())
            .await
            .unwrap();
        harness.fake().hold_quotes(false);
        let cancelled = harness
            .until(&task, |view| view["status"] == "cancelled")
            .await;
        assert_eq!(cancelled["status"], "cancelled");
        assert_eq!(
            harness.fake().submissions(),
            0,
            "cancelled during the held quote, before dispatch"
        );
        assert_eq!(
            harness.fake().cancellations(),
            0,
            "no external job existed to cancel"
        );
    })
    .await;
}

#[tokio::test]
async fn cancelling_a_dispatched_run_reaches_the_provider_and_keeps_its_receipt() {
    within(async {
        let harness = harness(false).await;
        let task = harness.run(IMAGE_MODEL, prompt()).await;
        let prediction = harness.parked(&task).await;
        let snapshot = harness.stored_until(&task, |_| true).await;
        // The caller cancels through Media's Task service, as `tasks/cancel` does.
        harness
            .tasks()
            .cancel_task(&forwarded_user(), task.clone())
            .await
            .unwrap();
        let cancelled = harness
            .until(&task, |view| view["status"] == "cancelled")
            .await;
        assert_eq!(cancelled["status"], "cancelled");
        assert_eq!(harness.fake().cancellations(), 1);
        let operation = harness
            .runtime()
            .operation(snapshot.task_id, &generation_key())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(operation.cancel_receipt.unwrap()["deleted"], true);
        assert_eq!(
            status_of(&harness, &task).await,
            PredictionStatus::Cancelled
        );

        // The provider's later report is stored as late and changes nothing.
        assert_eq!(
            harness
                .fake()
                .post_event(&prediction, "evt_late", true)
                .await
                .unwrap(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(harness.task(&task).await["status"], "cancelled");
        assert_eq!(harness.fake().submissions(), 1);
    })
    .await;
}

/// The forwarded identity of the test user, as the mesh forwards it to Media.
fn forwarded_user() -> veoveo_hub_sdk::HubInternalIdentity {
    let principal = testing::principal();
    let authority = testing::authority();
    let now = Utc::now();
    let context = HubRequestContext {
        audit: veoveo_audit_contract::AuditRequest::background(),
        access_token: AccessTokenSubject {
            issuer: principal.issuer.clone(),
            subject: principal.subject.clone(),
            oauth_client_id: OAuthClientId::parse(testing::TEST_CLIENT).unwrap(),
            session_family: None,
            audience: ProtectedResourceId::parse(testing::TEST_RESOURCE).unwrap(),
            work_context: authority.work_context.clone(),
            invocation_mode: InvocationMode::Direct,
            initiator: Some(principal.id.clone()),
            delegation_id: None,
            scopes: principal.scopes.clone(),
            jwt_id: None,
            issued_at: now,
            not_before: None,
            expires_at: now + TimeDelta::minutes(5),
        },
        principal: principal.clone(),
    };
    HubInternalTokenIssuer::new(
        TokenIssuer::parse(testing::TEST_ISSUER).unwrap(),
        testing::signing_key(),
    )
    .unwrap()
    .issue(ForwardedRequest {
        profile: HubProfileId::parse(MESH_PROFILE).unwrap(),
        server: veoveo_mcp_media::server(),
        audience: ResourceIndicator::parse(testing::TEST_RESOURCE).unwrap(),
        client: OAuthClientId::parse(testing::TEST_CLIENT).unwrap(),
        actor: principal,
        authority,
        permissions: BTreeSet::from([MediaPermission::GenerationsRun.name()]),
        request_context: Some(context),
        acting_server: None,
        acting_task: None,
        expires_at: now + TimeDelta::minutes(5),
    })
    .unwrap()
    .identity
}

#[tokio::test]
async fn a_run_cancelled_before_a_worker_claims_it_never_reaches_the_provider() {
    within(async {
        let mut harness = harness(true).await;
        harness.stop_worker().await;
        let task = harness.run(IMAGE_MODEL, prompt()).await;
        harness
            .tasks()
            .cancel_task(&forwarded_user(), task.clone())
            .await
            .unwrap();
        let stored = harness.stored_until(&task, |_| true).await;
        assert_eq!(stored.status, TaskStatus::Cancelled);
        assert_eq!(
            status_of(&harness, &task).await,
            PredictionStatus::Cancelled
        );
        harness.start_worker().await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(harness.fake().submissions(), 0);
        assert!(usage_of(&harness.task(&task).await).is_empty());
    })
    .await;
}

#[tokio::test]
async fn a_large_output_streams_into_artifact_in_parts_and_is_verified() {
    within(async {
        let harness = harness(true).await;
        let task = harness.run(LARGE_MODEL, prompt()).await;
        let completed = harness
            .until(&task, |view| view["status"] == "completed")
            .await;
        let artifacts = completed["result"]["structuredContent"]["metadata"]["artifacts"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0]["size"], LARGE_BYTES);
        assert_eq!(artifacts[0]["mediaType"], "video/mp4");
        let bytes = harness
            .download(artifacts[0]["artifactUri"].as_str().unwrap())
            .await;
        assert_eq!(bytes.len() as u64, LARGE_BYTES);
        assert!(bytes == expected(LARGE_BYTES), "the stored bytes differ");
    })
    .await;
}

#[tokio::test]
async fn an_output_redirected_off_the_allowed_hosts_fails_the_run_unfetched() {
    within(async {
        let harness = harness(true).await;
        // A listener under another host name counts every connection that reaches it.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let reached = Arc::new(AtomicU64::new(0));
        tokio::spawn({
            let reached = reached.clone();
            async move {
                while listener.accept().await.is_ok() {
                    reached.fetch_add(1, Ordering::SeqCst);
                }
            }
        });
        let elsewhere = url::Url::parse(&format!("http://localhost:{port}/output.png")).unwrap();
        harness.fake().redirect_outputs(Some(elsewhere));

        let task = harness.run(IMAGE_MODEL, prompt()).await;
        let failed = harness
            .until(&task, |view| view["status"] == "failed")
            .await;
        assert!(
            failed["statusMessage"]
                .as_str()
                .unwrap_or_default()
                .contains("redirected off the allowed hosts"),
            "{failed}"
        );
        let stored = harness
            .stored_until(&task, |snapshot| snapshot.status == TaskStatus::Failed)
            .await;
        assert_eq!(stored.error.unwrap().code, "output_not_stored");
        assert_eq!(status_of(&harness, &task).await, PredictionStatus::Failed);
        assert_eq!(reached.load(Ordering::SeqCst), 0);
    })
    .await;
}

#[tokio::test]
async fn a_provider_failure_fails_the_run_and_still_reconciles_its_bill() {
    within(async {
        let harness = harness(true).await;
        let task = harness.run(FAILING_MODEL, prompt()).await;
        let failed = harness
            .until(&task, |view| view["status"] == "failed")
            .await;
        assert!(
            failed["statusMessage"]
                .as_str()
                .unwrap_or_default()
                .contains("always fails"),
            "{failed}"
        );
        assert_eq!(status_of(&harness, &task).await, PredictionStatus::Failed);
        let usage = usage_of(&failed);
        assert_eq!(usage.len(), 1, "{failed}");
        assert_eq!(usage[0]["phase"], "actual");
    })
    .await;
}

#[tokio::test]
async fn a_caller_without_the_run_permission_starts_nothing() {
    within(async {
        let granted = vec![
            MediaPermission::ModelsRead.name(),
            MediaPermission::PredictionsRead.name(),
            ArtifactPermission::ArtifactsRead.name(),
            ArtifactPermission::ArtifactsWrite.name(),
        ];
        let harness = harness_granting(true, granted).await;
        let answer = harness
            .mesh
            .start_task(
                "media",
                "run",
                json!({"model": IMAGE_MODEL, "input": prompt()}),
            )
            .await;
        assert!(answer["error"].is_object(), "{answer}");
        assert_eq!(harness.fake().submissions(), 0);
        // The catalog stays readable.
        let models = harness
            .mesh
            .call("media", "models", json!({"filters":{}}))
            .await;
        assert_eq!(
            models["result"]["structuredContent"]["items"]
                .as_array()
                .unwrap()
                .len(),
            3,
            "{models}"
        );
    })
    .await;
}

#[tokio::test]
async fn the_operation_records_the_providers_prediction_as_its_job() {
    within(async {
        let harness = harness(false).await;
        let task = harness.run(IMAGE_MODEL, prompt()).await;
        let prediction = harness.parked(&task).await;
        let operation = harness
            .runtime()
            .operation(TaskId::parse(&task).unwrap(), &generation_key())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(operation.state, OperationState::Submitted);
        assert_eq!(operation.external_id.unwrap().as_str(), prediction.as_str());
        harness.fake().complete(&prediction).await.unwrap();
        harness
            .until(&task, |view| view["status"] == "completed")
            .await;
    })
    .await;
}

#[tokio::test]
async fn a_run_without_a_callback_completes_through_status_checks() {
    within(async {
        let harness = harness_with(false, Completion::Checks, all_permissions()).await;
        let task = harness.run(IMAGE_MODEL, prompt()).await;
        let prediction = harness.parked(&task).await;
        // The run sent no webhook URL, so the fake posts nothing; the run reads the
        // prediction's status endpoint and stays working while the prediction runs.
        let operation = harness
            .runtime()
            .operation(TaskId::parse(&task).unwrap(), &generation_key())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            operation.completion,
            veoveo_task::Completion::Checks(_)
        ));
        // The runtime's due pass wakes the run for its checks; it sleeps at most 5 s.
        tokio::time::timeout(Duration::from_secs(15), async {
            while harness.fake().status_reads() == 0 {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("the run checks the status endpoint");
        assert_eq!(harness.task(&task).await["status"], "working");
        assert!(harness.fake().finish(&prediction));
        let completed = harness
            .until(&task, |view| view["status"] == "completed")
            .await;
        let artifacts = completed["result"]["structuredContent"]["metadata"]["artifacts"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(
            harness
                .download(artifacts[0]["artifactUri"].as_str().unwrap())
                .await,
            expected(IMAGE_BYTES)
        );
        let usage = usage_of(&completed);
        assert_eq!(usage.len(), 1, "{completed}");
        assert_eq!(usage[0]["phase"], "actual");
        assert_eq!(harness.fake().submissions(), 1);
        assert_eq!(
            completed["result"]["structuredContent"]["metadata"]["prediction"]["completion"],
            "checks"
        );
        assert_eq!(
            prediction_of(&harness, &task)
                .await
                .summary()
                .unwrap()
                .completion,
            Some(veoveo_mcp_media::contract::CompletionMode::Checks)
        );
    })
    .await;
}

#[tokio::test]
async fn a_run_without_a_callback_finishes_on_its_own_when_the_provider_does() {
    within(async {
        let harness = harness_with(true, Completion::Checks, all_permissions()).await;
        let task = harness.run(IMAGE_MODEL, prompt()).await;
        let completed = harness
            .until(&task, |view| view["status"] == "completed")
            .await;
        assert_eq!(
            completed["result"]["structuredContent"]["metadata"]["artifacts"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    })
    .await;
}

#[tokio::test]
async fn a_restarted_fake_provider_repeats_no_identity_the_database_holds() {
    within(async {
        let mut harness = harness(true).await;
        let prediction_of = |view: &Value| {
            view["result"]["structuredContent"]["metadata"]["prediction"]["predictionId"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let first = harness.run(IMAGE_MODEL, prompt()).await;
        let first = harness
            .until(&first, |view| view["status"] == "completed")
            .await;

        // The installation restarts the fake against the same database. A repeated
        // prediction identity would collide with the first run's row, and a repeated
        // event identity would read as a repeat and never settle the second run.
        harness.restart_fake().await;
        assert_eq!(harness.fake().submissions(), 0);
        let second = harness.run(IMAGE_MODEL, prompt()).await;
        let completed = harness
            .until(&second, |view| view["status"] == "completed")
            .await;
        assert_ne!(prediction_of(&first), prediction_of(&completed));
        assert_eq!(prediction_of(&completed).len(), 32);
        let operation = harness
            .runtime()
            .operation(TaskId::parse(&second).unwrap(), &generation_key())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(operation.state, OperationState::Settled);
        assert_eq!(
            status_of(&harness, &second).await,
            PredictionStatus::Succeeded
        );
    })
    .await;
}

/// Make Media's database refuse every update that records a run's outputs, as a failing
/// database would. Media's group role owns the table.
async fn refuse_outputs(harness: &Harness) {
    let store = harness.db.server_store(&veoveo_mcp_media::server()).await;
    sqlx::raw_sql(
        "SET ROLE server_media;
         CREATE FUNCTION media.refuse_outputs() RETURNS trigger LANGUAGE plpgsql
             AS $$ BEGIN RAISE EXCEPTION 'injected database failure'; END $$;
         CREATE TRIGGER refuse_outputs BEFORE UPDATE ON media.prediction FOR EACH ROW
             WHEN (NEW.outputs <> OLD.outputs)
             EXECUTE FUNCTION media.refuse_outputs();
         RESET ROLE;",
    )
    .execute(store.pool())
    .await
    .unwrap();
}

#[tokio::test]
async fn a_provider_that_repeats_a_prediction_fails_the_second_run_as_its_task_ended() {
    within(async {
        let harness = harness(true).await;
        let first = harness.run(IMAGE_MODEL, prompt()).await;
        let completed = harness
            .until(&first, |view| view["status"] == "completed")
            .await;
        let held = MediaPredictionId::parse(
            completed["result"]["structuredContent"]["metadata"]["prediction"]["predictionId"]
                .as_str()
                .unwrap(),
        )
        .unwrap();

        // The provider names the first run's prediction again for the second run.
        harness.fake().name_next_prediction(&held);
        let second = harness.run(IMAGE_MODEL, prompt()).await;
        let stored = harness
            .stored_until(&second, |snapshot| snapshot.status == TaskStatus::Failed)
            .await;
        let failure = stored.error.unwrap();
        assert_eq!(failure.code, "prediction_conflict");
        assert!(
            failure.message.contains(held.as_str()),
            "{}",
            failure.message
        );
        // The second run reads as its Task ended, with the Task's failure.
        let second = prediction_of(&harness, &second).await;
        let summary = second.summary().unwrap();
        assert_eq!(summary.status, PredictionStatus::Failed);
        assert_eq!(summary.error, Some(failure.message.clone()));
        assert_eq!(summary.completed_at, stored.completed_at);

        // The first run keeps its prediction.
        let first_prediction = harness
            .predictions()
            .prediction(&owner(), &held)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            first_prediction.task.task_id,
            TaskId::parse(&first).unwrap()
        );
        assert_eq!(first_prediction.status(), PredictionStatus::Succeeded);
    })
    .await;
}

#[tokio::test]
async fn a_run_whose_outputs_the_database_refuses_reads_as_its_task_ended() {
    within(async {
        let harness = harness(true).await;
        // The database refuses to record the run's stored outputs.
        refuse_outputs(&harness).await;
        let task = harness.run(IMAGE_MODEL, prompt()).await;
        let stored = harness
            .stored_until(&task, |snapshot| snapshot.status == TaskStatus::Failed)
            .await;
        let failure = stored.error.clone().unwrap();
        assert_eq!(failure.code, "store_refused");
        assert!(!failure.message.contains("injected"), "{}", failure.message);

        // Every read shows the Task's end: nothing in Media copies it.
        let summary = prediction_of(&harness, &task).await.summary().unwrap();
        assert_eq!(summary.status, PredictionStatus::Failed);
        assert_eq!(summary.error, Some(failure.message.clone()));
        assert_eq!(summary.completed_at, stored.completed_at);
        let page = harness
            .predictions()
            .page(
                &owner(),
                &CollectionRequest::<PredictionCollection>::new(
                    NoFilters {},
                    PageSize::new(100).unwrap(),
                ),
            )
            .await
            .unwrap();
        assert_eq!(page.items().len(), 1);
        assert_eq!(page.items()[0].prediction.status, PredictionStatus::Failed);
    })
    .await;
}

#[tokio::test]
async fn a_run_the_provider_cannot_price_fails_before_it_reaches_the_provider() {
    within(async {
        let harness = harness(true).await;
        harness.fake().refuse_quotes(true);
        let task = harness.run(IMAGE_MODEL, prompt()).await;
        let failed = harness
            .until(&task, |view| view["status"] == "failed")
            .await;
        let stored = harness
            .stored_until(&task, |snapshot| snapshot.status == TaskStatus::Failed)
            .await;
        assert_eq!(stored.error.unwrap().code, "price_unavailable");
        assert!(usage_of(&failed).is_empty(), "{failed}");
        assert_eq!(status_of(&harness, &task).await, PredictionStatus::Failed);
        assert_eq!(harness.fake().submissions(), 0);
    })
    .await;
}

#[tokio::test]
async fn the_listing_shows_the_newest_run_first_as_its_task_stands() {
    within(async {
        let mut harness = harness(true).await;
        let first = harness.run(IMAGE_MODEL, prompt()).await;
        harness
            .until(&first, |view| view["status"] == "completed")
            .await;
        // The second run waits for a worker, so the listing shows it queued.
        harness.stop_worker().await;
        let second = harness.run(IMAGE_MODEL, prompt()).await;
        let selection =
            CollectionRequest::<PredictionCollection>::new(NoFilters {}, PageSize::new(1).unwrap());
        let page = harness
            .predictions()
            .page(&owner(), &selection)
            .await
            .unwrap();
        assert_eq!(page.items().len(), 1);
        assert_eq!(page.items()[0].prediction.task_id.to_string(), second);
        assert_eq!(page.items()[0].prediction.status, PredictionStatus::Queued);
        let cursor = page.next_cursor().unwrap().clone();
        let next = harness
            .predictions()
            .page(&owner(), &selection.continuing(cursor.clone()))
            .await
            .unwrap();
        assert_eq!(next.items().len(), 1);
        assert_eq!(next.items()[0].prediction.task_id.to_string(), first);
        assert_eq!(
            next.items()[0].prediction.status,
            PredictionStatus::Succeeded
        );
        assert!(next.next_cursor().is_none());

        let changed_limit =
            CollectionRequest::<PredictionCollection>::new(NoFilters {}, PageSize::new(2).unwrap())
                .continuing(cursor.clone());
        assert!(matches!(
            harness.predictions().page(&owner(), &changed_limit).await,
            Err(veoveo_mcp_media::predictions::PredictionError::StaleCursor)
        ));
        let mut foreign = owner();
        foreign.surface = veoveo_task::TaskSurface::parse("another-profile").unwrap();
        let continued =
            CollectionRequest::<PredictionCollection>::new(NoFilters {}, PageSize::new(1).unwrap())
                .continuing(cursor);
        assert!(matches!(
            harness.predictions().page(&foreign, &continued).await,
            Err(veoveo_mcp_media::predictions::PredictionError::StaleCursor)
        ));
    })
    .await;
}

#[tokio::test]
async fn model_tool_pages_the_synthetic_registry_and_refuses_changed_selection() {
    within(async {
        let harness = harness(true).await;
        let first = harness
            .mesh
            .call("media", "models", json!({"filters":{},"limit":2}))
            .await;
        let first = &first["result"]["structuredContent"];
        assert_eq!(first["items"].as_array().unwrap().len(), 2);
        let cursor = first["nextCursor"].as_str().unwrap();
        let second = harness
            .mesh
            .call(
                "media",
                "models",
                json!({"filters":{},"limit":2,"cursor":cursor}),
            )
            .await;
        let second = &second["result"]["structuredContent"];
        assert_eq!(second["items"].as_array().unwrap().len(), 1);
        assert!(second["nextCursor"].is_null());
        let ids: Vec<_> = first["items"]
            .as_array()
            .unwrap()
            .iter()
            .chain(second["items"].as_array().unwrap())
            .map(|item| item["modelId"].as_str().unwrap())
            .collect();
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
        let refused = harness
            .mesh
            .call(
                "media",
                "models",
                json!({"filters":{},"limit":1,"cursor":cursor}),
            )
            .await;
        assert!(refused["error"].is_object(), "{refused}");
    })
    .await;
}
