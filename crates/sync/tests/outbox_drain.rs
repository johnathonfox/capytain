// SPDX-License-Identifier: Apache-2.0

//! Integration tests for `qsl_sync::outbox_drain`.
//!
//! Drives the worker against a stub `MailBackend` + an in-memory
//! Turso DB. Exercises the success path, the retry-with-backoff
//! path, and the dead-letter-after-`MAX_ATTEMPTS` transition.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use tokio::runtime::Runtime;

use qsl_core::{
    Account, AccountId, AttachmentRef, BackendKind, Folder, FolderId, MailBackend, MailError,
    MessageBody, MessageFlags, MessageId, MessageList, SyncState,
};
use qsl_storage::{repos::outbox as outbox_repo, run_migrations, TursoConn};
use qsl_sync::outbox_drain::{
    self, BackendResolver, DeletePayload, DrainOutcome, MovePayload, UpdateFlagsPayload,
};

// ---------- Stub backend ----------

/// Backend that records every `update_flags` call and optionally
/// fails for the first N invocations to exercise the retry path.
type MoveLog = std::sync::Mutex<Vec<(Vec<MessageId>, FolderId)>>;
type DeleteLog = std::sync::Mutex<Vec<Vec<MessageId>>>;
type DraftLog = std::sync::Mutex<Vec<Vec<u8>>>;

struct FailingFlagsBackend {
    fail_count: AtomicUsize,
    success_count: AtomicUsize,
    fail_first_n: usize,
    /// When set, every `move_messages` call appends its args here
    /// for the test to inspect.
    move_observer: Option<MoveLog>,
    /// Same for `delete_messages`.
    delete_observer: Option<DeleteLog>,
    /// Records each `save_draft` call's raw bytes — exercises the
    /// `OP_SAVE_DRAFT` dispatch arm.
    save_draft_observer: Option<DraftLog>,
}

impl FailingFlagsBackend {
    fn new(fail_first_n: usize) -> Self {
        Self {
            fail_count: AtomicUsize::new(0),
            success_count: AtomicUsize::new(0),
            fail_first_n,
            move_observer: None,
            delete_observer: None,
            save_draft_observer: None,
        }
    }

    fn with_move_observer(mut self) -> Self {
        self.move_observer = Some(std::sync::Mutex::new(Vec::new()));
        self
    }

    fn with_delete_observer(mut self) -> Self {
        self.delete_observer = Some(std::sync::Mutex::new(Vec::new()));
        self
    }

    fn with_save_draft_observer(mut self) -> Self {
        self.save_draft_observer = Some(std::sync::Mutex::new(Vec::new()));
        self
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
    async fn move_messages(&self, ids: &[MessageId], target: &FolderId) -> Result<(), MailError> {
        if let Some(observer) = self.move_observer.as_ref() {
            observer
                .lock()
                .unwrap()
                .push((ids.to_vec(), target.clone()));
        }
        Ok(())
    }
    async fn delete_messages(&self, ids: &[MessageId]) -> Result<(), MailError> {
        if let Some(observer) = self.delete_observer.as_ref() {
            observer.lock().unwrap().push(ids.to_vec());
        }
        Ok(())
    }
    async fn save_draft(
        &self,
        raw: &[u8],
        _replace: Option<&MessageId>,
    ) -> Result<MessageId, MailError> {
        if let Some(observer) = self.save_draft_observer.as_ref() {
            observer.lock().unwrap().push(raw.to_vec());
        }
        Ok(MessageId("draft-saved".into()))
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
        signature: None,
        notify_enabled: true,
    };
    qsl_storage::repos::accounts::insert(conn, &acct)
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

#[test]
fn drain_dispatches_move_messages() {
    rt().block_on(async {
        let conn = TursoConn::in_memory().await.unwrap();
        run_migrations(&conn).await.unwrap();
        let account = seed_account(&conn).await;

        let payload = MovePayload {
            ids: vec![MessageId("m-1".into()), MessageId("m-2".into())],
            target: FolderId("Trash".into()),
        };
        outbox_repo::enqueue(
            &conn,
            &account,
            "move_messages",
            &serde_json::to_string(&payload).unwrap(),
        )
        .await
        .unwrap();

        let backend = Arc::new(FailingFlagsBackend::new(0).with_move_observer());
        let resolver = StubResolver {
            backend: backend.clone(),
        };
        let outcomes = outbox_drain::drain(&conn, &resolver, 32).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(&outcomes[0], DrainOutcome::Sent { .. }));

        let observed = backend.move_observer.as_ref().unwrap().lock().unwrap();
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].0, payload.ids);
        assert_eq!(observed[0].1, payload.target);
    });
}

#[test]
fn drain_dispatches_delete_messages() {
    rt().block_on(async {
        let conn = TursoConn::in_memory().await.unwrap();
        run_migrations(&conn).await.unwrap();
        let account = seed_account(&conn).await;

        let payload = DeletePayload {
            ids: vec![MessageId("m-1".into())],
        };
        outbox_repo::enqueue(
            &conn,
            &account,
            "delete_messages",
            &serde_json::to_string(&payload).unwrap(),
        )
        .await
        .unwrap();

        let backend = Arc::new(FailingFlagsBackend::new(0).with_delete_observer());
        let resolver = StubResolver {
            backend: backend.clone(),
        };
        let outcomes = outbox_drain::drain(&conn, &resolver, 32).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(&outcomes[0], DrainOutcome::Sent { .. }));

        let observed = backend.delete_observer.as_ref().unwrap().lock().unwrap();
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0], payload.ids);
    });
}

///  collapses repeated calls for the same key into a
/// single row. The compose pane's auto-save tick relies on this so a
/// burst of typing produces one APPEND per drain cycle rather than
/// one per keystroke.
#[test]
fn enqueue_dedup_collapses_repeated_calls() {
    rt().block_on(async {
        let conn = TursoConn::in_memory().await.unwrap();
        run_migrations(&conn).await.unwrap();
        let account = seed_account(&conn).await;

        let id1 = outbox_repo::enqueue_dedup(
            &conn,
            &account,
            "save_draft",
            r#"{"version":1}"#,
            "draft-7",
        )
        .await
        .unwrap();
        let id2 = outbox_repo::enqueue_dedup(
            &conn,
            &account,
            "save_draft",
            r#"{"version":2}"#,
            "draft-7",
        )
        .await
        .unwrap();
        assert_eq!(id1, id2, "same dedup_key must reuse the row");

        // Different key → different row.
        let id3 = outbox_repo::enqueue_dedup(
            &conn,
            &account,
            "save_draft",
            r#"{"version":1}"#,
            "draft-8",
        )
        .await
        .unwrap();
        assert_ne!(id1, id3);

        let due = outbox_repo::list_due(&conn, Utc::now(), 32).await.unwrap();
        assert_eq!(due.len(), 2, "only two distinct rows after three enqueues");
        let row7 = due.iter().find(|e| e.id == id1).unwrap();
        assert!(
            row7.payload_json.contains("\"version\":2"),
            "latest payload must win: {}",
            row7.payload_json
        );
    });
}

/// End-to-end: enqueue a save_draft row + drain it; the backend's
/// save_draft observer should see the decoded RFC 5322 bytes.
#[test]
fn drain_dispatches_save_draft_with_raw_bytes() {
    use base64::engine::general_purpose::STANDARD as base64_engine;
    use base64::Engine as _;
    rt().block_on(async {
        let conn = TursoConn::in_memory().await.unwrap();
        run_migrations(&conn).await.unwrap();
        let account = seed_account(&conn).await;

        let raw = b"From: test
Subject: hi

body
";
        let payload = serde_json::json!({
            "draft_id": "draft-99",
            "raw_b64": base64_engine.encode(raw),
        });
        outbox_repo::enqueue_dedup(
            &conn,
            &account,
            outbox_drain::OP_SAVE_DRAFT,
            &payload.to_string(),
            "draft-99",
        )
        .await
        .unwrap();

        let backend = Arc::new(FailingFlagsBackend::new(0).with_save_draft_observer());
        let resolver = StubResolver {
            backend: backend.clone(),
        };
        let outcomes = outbox_drain::drain(&conn, &resolver, 32).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(&outcomes[0], DrainOutcome::Sent { .. }));

        let observed = backend
            .save_draft_observer
            .as_ref()
            .unwrap()
            .lock()
            .unwrap();
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0], raw.to_vec());
    });
}

/// `OP_SAVE_DRAFT` must read `drafts.server_id` at execution time
/// and feed it to the backend as `replace`, then persist the
/// returned id. This is the dedup contract — without it the server
/// would accumulate one draft copy per save.
#[test]
fn save_draft_threads_server_id_through_dispatch() {
    use base64::engine::general_purpose::STANDARD as base64_engine;
    use base64::Engine as _;
    use qsl_core::{Draft, DraftBodyKind, DraftId, EmailAddress};
    use qsl_storage::repos::drafts as drafts_repo;

    /// Stub backend that asserts the prior `replace` it gets matches
    /// the `expected_prior` it was constructed with, then returns
    /// the configured `next_id`.
    struct DraftDedupBackend {
        expected_prior: Option<MessageId>,
        next_id: MessageId,
    }
    #[async_trait]
    impl MailBackend for DraftDedupBackend {
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
            unimplemented!()
        }
        async fn move_messages(&self, _: &[MessageId], _: &FolderId) -> Result<(), MailError> {
            unimplemented!()
        }
        async fn delete_messages(&self, _: &[MessageId]) -> Result<(), MailError> {
            unimplemented!()
        }
        async fn save_draft(
            &self,
            _raw: &[u8],
            replace: Option<&MessageId>,
        ) -> Result<MessageId, MailError> {
            assert_eq!(replace.cloned(), self.expected_prior);
            Ok(self.next_id.clone())
        }
        async fn submit_message(&self, _: &[u8]) -> Result<Option<MessageId>, MailError> {
            unimplemented!()
        }
    }
    struct DraftResolver {
        backend: Arc<DraftDedupBackend>,
    }
    #[async_trait]
    impl BackendResolver for DraftResolver {
        async fn open(&self, _: &AccountId) -> Result<Arc<dyn MailBackend>, MailError> {
            Ok(self.backend.clone())
        }
    }

    rt().block_on(async {
        let conn = TursoConn::in_memory().await.unwrap();
        run_migrations(&conn).await.unwrap();
        let account = seed_account(&conn).await;

        // Seed a local draft. No server_id yet.
        let draft = Draft {
            id: DraftId("dr-test1".into()),
            account_id: account.clone(),
            in_reply_to: None,
            references: vec![],
            to: vec![EmailAddress {
                address: "to@example.com".into(),
                display_name: None,
            }],
            cc: vec![],
            bcc: vec![],
            subject: "test".into(),
            body: "body".into(),
            body_kind: DraftBodyKind::Plain,
            attachments: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        drafts_repo::save(&conn, &draft).await.unwrap();

        // Cycle 1: no prior. Backend asserts replace == None.
        let raw = b"From: a\r\nMessage-ID: <m1@x>\r\nTo: b\r\nSubject: x\r\n\r\nbody\r\n";
        let payload = serde_json::json!({
            "draft_id": draft.id.0,
            "raw_b64": base64_engine.encode(raw),
        });
        outbox_repo::enqueue_dedup(
            &conn,
            &account,
            outbox_drain::OP_SAVE_DRAFT,
            &payload.to_string(),
            &draft.id.0,
        )
        .await
        .unwrap();

        let backend = Arc::new(DraftDedupBackend {
            expected_prior: None,
            next_id: MessageId("server-v1".into()),
        });
        let resolver = DraftResolver {
            backend: backend.clone(),
        };
        let outcomes = outbox_drain::drain(&conn, &resolver, 32).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(&outcomes[0], DrainOutcome::Sent { .. }));

        // server_id was written.
        let stored = drafts_repo::get_server_id(&conn, &draft.id).await.unwrap();
        assert_eq!(stored, Some(MessageId("server-v1".into())));

        // Cycle 2: backend now expects replace == Some("server-v1").
        outbox_repo::enqueue_dedup(
            &conn,
            &account,
            outbox_drain::OP_SAVE_DRAFT,
            &payload.to_string(),
            &draft.id.0,
        )
        .await
        .unwrap();
        let backend2 = Arc::new(DraftDedupBackend {
            expected_prior: Some(MessageId("server-v1".into())),
            next_id: MessageId("server-v2".into()),
        });
        let resolver2 = DraftResolver {
            backend: backend2.clone(),
        };
        let outcomes = outbox_drain::drain(&conn, &resolver2, 32).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(&outcomes[0], DrainOutcome::Sent { .. }));

        let stored = drafts_repo::get_server_id(&conn, &draft.id).await.unwrap();
        assert_eq!(stored, Some(MessageId("server-v2".into())));
    });
}
