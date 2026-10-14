//! Media's run rows on a fresh database migrated as the server's own login: recording a
//! run, its submission and its outputs, reads by Task and by prediction, a provider's
//! repeated prediction, and the sweep's walk through runs past their retention. Who may
//! see a run, and how it stands, is its Task's, which `tests/run.rs` covers.
use std::time::Duration;

use chrono::{TimeDelta, Utc};
use veoveo_mcp_media::{
    contract::{CompletionMode, GeneratedArtifact, MediaModelId, MediaPredictionId},
    store::{MediaStore, MediaStoreError, SWEEP_BATCH, stored_tenant},
};
use veoveo_platform_store::TenantId as StoredTenant;
use veoveo_test_fixtures::store::TestDb;
use veoveo_types::{ResourceUri, TaskId, TenantId};

async fn with_store<F, Fut>(test: F)
where
    F: FnOnce(MediaStore) -> Fut,
    Fut: Future<Output = ()>,
{
    tokio::time::timeout(Duration::from_secs(60), async {
        let db = TestDb::new().await;
        db.migrate_server(&veoveo_mcp_media::server(), veoveo_mcp_media::migrator())
            .await;
        let store = MediaStore::new(db.server_store(&veoveo_mcp_media::server()).await);
        test(store).await;
        drop(db);
    })
    .await
    .expect("the store test exceeded 60 seconds");
}

fn tenant(name: &str) -> StoredTenant {
    stored_tenant(&TenantId::parse(name).unwrap()).unwrap()
}

fn model() -> MediaModelId {
    MediaModelId::parse("fake/text-to-image").unwrap()
}

fn prediction(id: &str) -> MediaPredictionId {
    MediaPredictionId::parse(id).unwrap()
}

#[tokio::test]
async fn a_run_records_its_submission_and_outputs() {
    with_store(|store| async move {
        let acme = tenant("acme");
        let task = TaskId::new();
        let now = Utc::now();
        store.record_run(acme, task, &model(), now).await.unwrap();
        // A repeat of the start changes nothing.
        store.record_run(acme, task, &model(), now).await.unwrap();
        let row = store.run(acme, task).await.unwrap().unwrap();
        assert_eq!(row.model_id().unwrap(), model());
        assert_eq!(row.prediction_id().unwrap(), None);
        assert!(row.outputs().is_empty());

        store
            .record_submission(
                acme,
                task,
                Some(&prediction("p-1")),
                Some("http://provider.test/api/v3/predictions/p-1/result"),
                None,
            )
            .await
            .unwrap();
        // A later call without the prediction keeps it and adds the completion.
        store
            .record_submission(acme, task, None, None, Some(CompletionMode::Checks))
            .await
            .unwrap();
        let row = store.run(acme, task).await.unwrap().unwrap();
        assert_eq!(row.prediction_id().unwrap(), Some(prediction("p-1")));
        assert_eq!(
            row.status_url.as_deref(),
            Some("http://provider.test/api/v3/predictions/p-1/result")
        );
        assert_eq!(row.completion().unwrap(), Some(CompletionMode::Checks));
        assert_eq!(store.status_url(task).await.unwrap(), row.status_url);

        let output = GeneratedArtifact {
            index: 0,
            artifact_uri: ResourceUri::new("artifact://artifacts/a1").unwrap(),
            media_type: "image/png".into(),
            size: Some(4),
        };
        store
            .record_outputs(
                acme,
                task,
                &prediction("p-1"),
                std::slice::from_ref(&output),
            )
            .await
            .unwrap();
        let row = store
            .by_prediction(acme, &prediction("p-1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.task_id(), task);
        assert_eq!(row.outputs(), [output]);
        // Another tenant has no such prediction.
        assert!(
            store
                .by_prediction(tenant("globex"), &prediction("p-1"))
                .await
                .unwrap()
                .is_none()
        );
    })
    .await;
}

#[tokio::test]
async fn a_repeated_prediction_is_a_conflict_and_reads_are_per_tenant() {
    with_store(|store| async move {
        let (acme, globex) = (tenant("acme"), tenant("globex"));
        let (first, second, other) = (TaskId::new(), TaskId::new(), TaskId::new());
        let now = Utc::now();
        for (tenant, task) in [(acme, first), (acme, second), (globex, other)] {
            store.record_run(tenant, task, &model(), now).await.unwrap();
        }
        store
            .record_submission(acme, first, Some(&prediction("p-1")), None, None)
            .await
            .unwrap();
        let conflict = store
            .record_submission(acme, second, Some(&prediction("p-1")), None, None)
            .await
            .unwrap_err();
        assert!(
            matches!(&conflict, MediaStoreError::PredictionConflict(id) if *id == prediction("p-1")),
            "{conflict}"
        );
        // Another tenant may hold the same prediction identity.
        store
            .record_submission(globex, other, Some(&prediction("p-1")), None, None)
            .await
            .unwrap();

        let mut runs = store
            .runs(acme, &[first, second, other, TaskId::new()])
            .await
            .unwrap();
        runs.sort_by_key(|row| row.task_id);
        let mut expected = vec![first, second];
        expected.sort();
        assert_eq!(
            runs.iter().map(|row| row.task_id()).collect::<Vec<_>>(),
            expected
        );
    })
    .await;
}

#[tokio::test]
async fn the_sweep_walks_runs_past_their_retention_and_deletes_one_at_a_time() {
    with_store(|store| async move {
        let acme = tenant("acme");
        let now = Utc::now();
        let old: Vec<TaskId> = (0..3).map(|_| TaskId::new()).collect();
        for (age, task) in old.iter().enumerate() {
            let days = 40 + i64::try_from(age).unwrap();
            store
                .record_run(acme, *task, &model(), now - TimeDelta::days(days))
                .await
                .unwrap();
        }
        let recent = TaskId::new();
        store.record_run(acme, recent, &model(), now).await.unwrap();

        // Runs past the cutoff come oldest first, and a keyset continues after a batch.
        let cutoff = now - TimeDelta::days(30);
        let batch = store.past_retention(cutoff, None).await.unwrap();
        assert!(batch.len() as i64 <= SWEEP_BATCH);
        assert_eq!(
            batch.iter().map(|run| run.task_id).collect::<Vec<_>>(),
            vec![old[2], old[1], old[0]]
        );
        let after = store.past_retention(cutoff, Some(&batch[1])).await.unwrap();
        assert_eq!(
            after.iter().map(|run| run.task_id).collect::<Vec<_>>(),
            vec![old[0]]
        );

        assert!(store.delete_run(acme, old[1]).await.unwrap());
        assert!(!store.delete_run(acme, old[1]).await.unwrap());
        assert!(store.run(acme, old[1]).await.unwrap().is_none());
        assert!(store.run(acme, recent).await.unwrap().is_some());
    })
    .await;
}
