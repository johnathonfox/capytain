// SPDX-License-Identifier: Apache-2.0

//! Integration tests for `capytain_sync::outbox_drain`.
//!
//! Drives the worker against a stub `MailBackend` + an in-memory
//! Turso DB. Exercises the success path, the retry-with-backoff
//! path, and the dead-letter-after-`MAX_ATTEMPTS` transition.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use tokio::runtime::Runtime;

use capytain_core::{
    Account, AccountId, AttachmentRef, BackendKind, Folder, FolderId, MailBackend, MailError,
    MessageBody, MessageFlags, MessageId, MessageList, SyncState,
};
use capytain_storage::{repos::outbox as outbox_repo, run_migrations, TursoConn};
use capytain_sync::outbox_drain::{self, BackendResolver, DrainOutcome, UpdateFlagsPayload};

// ---------- Stub backend ----------

/// Backend that records every `update_flags` call and optionally
/// fails for the first N invocations to exercise the retry path.
struct FailingFlagsBackend {
    fail_count: AtomicUsize,
    success_count: AtomicUsize,
    fail_first_n: usize,
}

impl FailingFlagsBackend {
    fn new(fail_first_n: usize) -> Self {
        Self {
            fail_count: AtomicUsize::new(0),
            success_count: AtomicUsize::new(0),
            fail_first_n,
        }
    }
}

#[async_trait]
impl MailBackend for FailingFlagsBackend {
    async fn list_folders(&self) -> Result<Vec<Folder>, MailError> {
        Ok(vec![])
    }
    async fn list_messages(
        &self,
        _: &FolderId,
        _: Option<&SyncState>,
        _: Option<u32>,
    ) -> Result<MessageList, MailError> {
        unimplemented!()
    }
    async fn fetch_message(&self, _: &MessageId) -> Result<MessageBody, MailError> {
        unimplemented!()
    }
    async fn fetch_attachment(
        &self,
        _: &MessageId,
        _: &AttachmentRef,
    ) -> Result<Vec<u8>, MailError> {
        unimplemented!()
    }
    async fn update_flags(
        &self,
        _: &[MessageId],
        _: MessageFlags,
        _: MessageFlags,
    ) -> Result<(), MailError> {
        let n = self.fail_count.load(Ordering::SeqCst);
        if n < self.fail_first_n {
            self.fail_count.fetch_add(1, Ordering::SeqCst);
            return Err(MailError::Protocol("scripted failure".into()));
        }
        self.success_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn move_messages(&self, _: &[MessageId], _: &FolderId) -> Result<(), MailError> {
        unimplemented!()
    }
    async fn delete_messages(&self, _: &[MessageId]) -> Result<(), MailError> {
        unimplemented!()
    }
    async fn save_draft(&self, _: &[u8]) -> Result<MessageId, MailError> {
        unimplemented!()
    }
    async fn submit_message(&self, _: &[u8]) -> Result<Option<MessageId>, MailError> {
        unimplemented!()
    }
}

struct StubResolver {
    backend: Arc<FailingFlagsBackend>,
}

#[async_trait]
impl BackendResolver for StubResolver {
    async fn open(&self, _account: &AccountId) -> Result<Arc<dyn MailBackend>, MailError> {
        Ok(self.backend.clone())
    }
}

fn rt() -> Runtime {
    Runtime::new().unwrap()
}

async fn seed_account(conn: &TursoConn) -> AccountId {
    let acct = Account {
        id: AccountId("acct".into()),
        kind: BackendKind::ImapSmtp,
        display_name: "x".into(),
        email_address: "x@example.com".into(),
        created_at: Utc::now(),
    };
    capytain_storage::repos::accounts::insert(conn, &acct)
        .await
        .unwrap();
    acct.id
}

fn payload(seen: bool) -> String {
    let p = UpdateFlagsPayload {
        ids: vec![MessageId("m-1".into())],
        add: MessageFlags {
            seen,
            ..Default::default()
        },
        remove: MessageFlags {
            seen: !seen,
            ..Default::default()
        },
    };
    serde_json::to_string(&p).unwrap()
}

// ---------- Tests ----------

#[test]
fn drain_success_deletes_row() {
    rt().block_on(async {
        let conn = TursoConn::in_memory().await.unwrap();
        run_migrations(&conn).await.unwrap();
        let account = seed_account(&conn).await;
        outbox_repo::enqueue(&conn, &account, "update_flags", &payload(true))
            .await
            .unwrap();

        let backend = Arc::new(FailingFlagsBackend::new(0));
        let resolver = StubResolver {
            backend: backend.clone(),
        };
        let outcomes = outbox_drain::drain(&conn, &resolver, 32).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(&outcomes[0], DrainOutcome::Sent { .. }));
        assert_eq!(backend.success_count.load(Ordering::SeqCst), 1);

        // Row is gone.
        let again = outbox_drain::drain(&conn, &resolver, 32).await.unwrap();
        assert!(again.is_empty());
    });
}

#[test]
fn drain_failure_schedules_retry() {
    rt().block_on(async {
        let conn = TursoConn::in_memory().await.unwrap();
        run_migrations(&conn).await.unwrap();
        let account = seed_account(&conn).await;
        outbox_repo::enqueue(&conn, &account, "update_flags", &payload(true))
            .await
            .unwrap();

        let backend = Arc::new(FailingFlagsBackend::new(1));
        let resolver = StubResolver {
            backend: backend.clone(),
        };
        let outcomes = outbox_drain::drain(&conn, &resolver, 32).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            DrainOutcome::Retrying {
                attempts_after,
                error,
                ..
            } => {
                assert_eq!(*attempts_after, 1);
                assert!(error.contains("scripted failure"));
            }
            other => panic!("expected Retrying, got {other:?}"),
        }

        // Row is still there but its next_attempt_at is in the
        // future, so an immediate-now drain finds nothing due.
        let again = outbox_drain::drain(&conn, &resolver, 32).await.unwrap();
        assert!(again.is_empty());
    });
}

#[test]
fn drain_dead_letters_on_final_failure() {
    rt().block_on(async {
        let conn = TursoConn::in_memory().await.unwrap();
        run_migrations(&conn).await.unwrap();
        let account = seed_account(&conn).await;
        let id = outbox_repo::enqueue(&conn, &account, "update_flags", &payload(true))
            .await
            .unwrap();

        // Push the row to attempts = MAX_ATTEMPTS - 1 by recording
        // failures with the correct prev_attempts each time.
        // record_failure(prev=4) is the boundary where the row DLQs;
        // we stop one before that so the *drain* triggers the
        // transition.
        // Pass a `now` deep in the past so each prep failure's
        // backoff schedule still lands behind `Utc::now()`, keeping
        // the row visible to the live drain.
        let backdated = Utc::now() - chrono::Duration::days(1);
        for prev in 0..(outbox_repo::MAX_ATTEMPTS - 1) {
            outbox_repo::record_failure(&conn, &id, prev, "prep", backdated)
                .await
                .unwrap();
        }

        let backend = Arc::new(FailingFlagsBackend::new(usize::MAX));
        let resolver = StubResolver {
            backend: backend.clone(),
        };
        let outcomes = outbox_drain::drain(&conn, &resolver, 32).await.unwrap();
        assert_eq!(outcomes.len(), 1, "row should have been picked up");
        assert!(
            matches!(&outcomes[0], DrainOutcome::DeadLettered { .. }),
            "expected DLQ, got {:?}",
            outcomes[0]
        );

        let dlq = outbox_repo::list_dlq(&conn).await.unwrap();
        assert_eq!(dlq.len(), 1);
        assert_eq!(dlq[0].id, id);
        assert!(dlq[0].next_attempt_at.is_none());
    });
}
