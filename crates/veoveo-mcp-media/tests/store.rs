//! Media's run rows on a fresh database migrated as the server's own login: recording,
//! progress and endings, reads filtered by owner and labels, keyset pages and the sweep.
use std::{collections::BTreeSet, time::Duration};

use chrono::{TimeDelta, Utc};
use veoveo_mcp_media::{
    contract::{
        CompletionMode, GeneratedArtifact, ListCursor, MediaModelId, MediaPredictionId,
        PredictionStatus,
    },
    store::{MediaStore, PredictionPageSize, RunEnding, RunOwner},
};
use veoveo_test_fixtures::store::TestDb;
use veoveo_types::{DataLabelId, PrincipalId, ResourceUri, TaskId, TenantId};

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

fn owner(tenant: &str, principal: &str, labels: &[&str]) -> RunOwner {
    RunOwner::new(
        &TenantId::parse(tenant).unwrap(),
        &PrincipalId::parse(principal).unwrap(),
        labels
            .iter()
            .map(|label| DataLabelId::parse(*label).unwrap())
            .collect::<BTreeSet<_>>(),
    )
    .unwrap()
}

fn model() -> MediaModelId {
    MediaModelId::parse("fake/text-to-image").unwrap()
}

fn prediction(id: &str) -> MediaPredictionId {
    MediaPredictionId::parse(id).unwrap()
}

fn page(size: u32) -> PredictionPageSize {
    PredictionPageSize::new(size).unwrap()
}

#[tokio::test]
async fn a_run_moves_through_its_states_and_keeps_its_first_ending() {
    with_store(|store| async move {
        let ada = owner("acme", "ada", &[]);
        let task = TaskId::new();
        let now = Utc::now();
        store.record_run(&ada, task, &model(), now).await.unwrap();
        // A repeat of the start changes nothing.
        store.record_run(&ada, task, &model(), now).await.unwrap();
        let row = store.run(ada.tenant(), task).await.unwrap().unwrap();
        assert_eq!(row.status().unwrap(), PredictionStatus::Queued);
        store
            .mark_processing(
                ada.tenant(),
                task,
                Some(&prediction("p-1")),
                Some("http://provider.test/api/v3/predictions/p-1/result"),
                Some(CompletionMode::Checks),
                now,
            )
            .await
            .unwrap();
        let processing = store.run(ada.tenant(), task).await.unwrap().unwrap();
        assert_eq!(
            processing.summary().unwrap().completion,
            Some(CompletionMode::Checks)
        );
        store
            .mark_storing(ada.tenant(), task, &prediction("p-1"), now)
            .await
            .unwrap();
        let output = GeneratedArtifact {
            index: 0,
            artifact_uri: ResourceUri::new(
                "artifact://artifacts/0199d4a1-7c00-7b3e-9f2a-6c1d2e3f4a5b",
            )
            .unwrap(),
            media_type: "image/png".into(),
            size: Some(10),
        };
        store
            .finish(
                ada.tenant(),
                task,
                Some(&prediction("p-1")),
                &RunEnding::Succeeded(vec![output.clone()]),
                now,
            )
            .await
            .unwrap();
        store
            .finish(
                ada.tenant(),
                task,
                None,
                &RunEnding::Failed("too late".into()),
                now,
            )
            .await
            .unwrap();
        assert_eq!(
            store.status_url(task).await.unwrap().as_deref(),
            Some("http://provider.test/api/v3/predictions/p-1/result")
        );
        let row = store.run(ada.tenant(), task).await.unwrap().unwrap();
        let summary = row.summary().unwrap();
        assert_eq!(summary.status, PredictionStatus::Succeeded);
        assert_eq!(summary.prediction_id, Some(prediction("p-1")));
        assert_eq!(summary.output_count, 1);
        assert_eq!(row.outputs(), [output]);
        assert_eq!(summary.error, None);
    })
    .await;
}

#[tokio::test]
async fn reads_show_only_the_owners_runs_whose_labels_they_hold() {
    with_store(|store| async move {
        let now = Utc::now();
        let ada = owner("acme", "ada", &["secret"]);
        let ada_plain = owner("acme", "ada", &[]);
        let bob = owner("acme", "bob", &["secret"]);
        let other_tenant = owner("other", "ada", &["secret"]);
        let task = TaskId::new();
        store.record_run(&ada, task, &model(), now).await.unwrap();
        store
            .mark_processing(
                ada.tenant(),
                task,
                Some(&prediction("p-2")),
                None,
                None,
                now,
            )
            .await
            .unwrap();
        assert!(
            store
                .prediction(&ada, &prediction("p-2"))
                .await
                .unwrap()
                .is_some()
        );
        for reader in [&ada_plain, &bob, &other_tenant] {
            assert!(
                store
                    .prediction(reader, &prediction("p-2"))
                    .await
                    .unwrap()
                    .is_none(),
                "{reader:?}"
            );
            assert!(
                store
                    .predictions_page(reader, None, page(10))
                    .await
                    .unwrap()
                    .items
                    .is_empty()
            );
        }
        assert_eq!(
            store
                .predictions_page(&ada, None, page(10))
                .await
                .unwrap()
                .items
                .len(),
            1
        );
    })
    .await;
}

#[tokio::test]
async fn pages_run_newest_first_and_a_cursor_pages_only_its_own_listing() {
    with_store(|store| async move {
        let now = Utc::now();
        let ada = owner("acme", "ada", &[]);
        let mut tasks = Vec::new();
        for _ in 0..5 {
            let task = TaskId::new();
            store.record_run(&ada, task, &model(), now).await.unwrap();
            tasks.push(task);
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let first = store.predictions_page(&ada, None, page(2)).await.unwrap();
        assert_eq!(
            first
                .items
                .iter()
                .map(|row| row.task_id())
                .collect::<Vec<_>>(),
            [tasks[4], tasks[3]]
        );
        let cursor = first.next.clone().unwrap();
        let second = store
            .predictions_page(&ada, Some(&cursor), page(2))
            .await
            .unwrap();
        assert_eq!(
            second
                .items
                .iter()
                .map(|row| row.task_id())
                .collect::<Vec<_>>(),
            [tasks[2], tasks[1]]
        );
        let last = store
            .predictions_page(&ada, second.next.as_ref(), page(2))
            .await
            .unwrap();
        assert_eq!(last.items.len(), 1);
        assert_eq!(last.next, None);
        // Another reader's cursor and a malformed one are refused.
        let bob = owner("acme", "bob", &[]);
        assert!(
            store
                .predictions_page(&bob, Some(&cursor), page(2))
                .await
                .is_err()
        );
        let garbage = ListCursor::parse("abc").unwrap();
        assert!(
            store
                .predictions_page(&ada, Some(&garbage), page(2))
                .await
                .is_err()
        );
    })
    .await;
}

#[tokio::test]
async fn the_sweep_prunes_only_runs_that_ended_before_the_cutoff() {
    with_store(|store| async move {
        let ada = owner("acme", "ada", &[]);
        let now = Utc::now();
        let (old, recent, open) = (TaskId::new(), TaskId::new(), TaskId::new());
        for task in [old, recent, open] {
            store.record_run(&ada, task, &model(), now).await.unwrap();
        }
        store
            .finish(
                ada.tenant(),
                old,
                None,
                &RunEnding::Unresolved,
                now - TimeDelta::days(40),
            )
            .await
            .unwrap();
        store
            .finish(
                ada.tenant(),
                recent,
                None,
                &RunEnding::Cancelled(Vec::new()),
                now,
            )
            .await
            .unwrap();
        assert_eq!(store.prune(now - TimeDelta::days(30)).await.unwrap(), 1);
        assert_eq!(store.prune(now - TimeDelta::days(30)).await.unwrap(), 0);
        assert!(store.run(ada.tenant(), old).await.unwrap().is_none());
        assert!(store.run(ada.tenant(), recent).await.unwrap().is_some());
        assert!(store.run(ada.tenant(), open).await.unwrap().is_some());
    })
    .await;
}
