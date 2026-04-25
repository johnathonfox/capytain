// SPDX-License-Identifier: Apache-2.0

//! Integration tests for `capytain_sync::sync_folder`.
//!
//! Drives the function with a stub `MailBackend` against an in-memory
//! Turso database. The IMAP/JMAP adapters' own protocol behavior is
//! tested elsewhere; here we only care that `sync_folder` walks the
//! header upsert + cursor-persist loop correctly.

use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use tokio::runtime::Runtime;

use capytain_core::{
    Account, AccountId, AttachmentRef, BackendKind, EmailAddress, Folder, FolderId, FolderRole,
    MailBackend, MailError, MessageBody, MessageFlags, MessageHeaders, MessageId, MessageList,
    SyncState,
};
use capytain_storage::{
    repos::{folders as folders_repo, messages as messages_repo, sync_states as sync_states_repo},
    run_migrations, BlobStore, TursoConn,
};

use capytain_sync::sync_folder;

// ---------- Stub backend ----------

/// Minimal `MailBackend` that returns a scripted sequence of
/// `MessageList` responses and a per-`MessageId` map of raw bytes.
/// Each call to `list_messages` consumes one response from the front
/// of the queue; `fetch_raw_message` reads from `raw_bodies`.
struct StubBackend {
    folders: Vec<Folder>,
    responses: Mutex<Vec<MessageList>>,
    raw_bodies: std::collections::HashMap<String, Vec<u8>>,
    /// IDs that should fail their body fetch — simulates a UIDVALIDITY
    /// race or a server hiccup mid-cycle.
    failing_ids: std::collections::HashSet<String>,
}

#[async_trait]
impl MailBackend for StubBackend {
    async fn list_folders(&self) -> Result<Vec<Folder>, MailError> {
        Ok(self.folders.clone())
    }

    async fn list_messages(
        &self,
        _folder: &FolderId,
        _since: Option<&SyncState>,
        _limit: Option<u32>,
    ) -> Result<MessageList, MailError> {
        let mut q = self.responses.lock().unwrap();
        if q.is_empty() {
            Err(MailError::Other("stub: no scripted responses left".into()))
        } else {
            Ok(q.remove(0))
        }
    }

    async fn fetch_message(&self, _id: &MessageId) -> Result<MessageBody, MailError> {
        unimplemented!("sync_folder uses fetch_raw_message; fetch_message isn't exercised")
    }

    async fn fetch_raw_message(&self, id: &MessageId) -> Result<Vec<u8>, MailError> {
        if self.failing_ids.contains(&id.0) {
            return Err(MailError::Protocol(format!(
                "stub: scripted failure for {}",
                id.0
            )));
        }
        self.raw_bodies
            .get(&id.0)
            .cloned()
            .ok_or_else(|| MailError::NotFound(id.0.clone()))
    }

    async fn fetch_attachment(
        &self,
        _message: &MessageId,
        _attachment: &AttachmentRef,
    ) -> Result<Vec<u8>, MailError> {
        unimplemented!()
    }

    async fn update_flags(
        &self,
        _messages: &[MessageId],
        _add: MessageFlags,
        _remove: MessageFlags,
    ) -> Result<(), MailError> {
        unimplemented!()
    }

    async fn move_messages(
        &self,
        _messages: &[MessageId],
        _target: &FolderId,
    ) -> Result<(), MailError> {
        unimplemented!()
    }

    async fn delete_messages(&self, _messages: &[MessageId]) -> Result<(), MailError> {
        unimplemented!()
    }

    async fn save_draft(&self, _raw_rfc822: &[u8]) -> Result<MessageId, MailError> {
        unimplemented!()
    }

    async fn submit_message(&self, _raw_rfc822: &[u8]) -> Result<Option<MessageId>, MailError> {
        unimplemented!()
    }
}

// ---------- Fixtures ----------

fn rt() -> Runtime {
    Runtime::new().unwrap()
}

fn header(id: &str, account: &AccountId, folder: &FolderId, subject: &str) -> MessageHeaders {
    MessageHeaders {
        id: MessageId(id.into()),
        account_id: account.clone(),
        folder_id: folder.clone(),
        thread_id: None,
        rfc822_message_id: None,
        subject: subject.into(),
        from: vec![EmailAddress {
            address: "sender@example.com".into(),
            display_name: None,
        }],
        reply_to: vec![],
        to: vec![],
        cc: vec![],
        bcc: vec![],
        date: Utc.with_ymd_and_hms(2026, 4, 24, 12, 0, 0).unwrap(),
        flags: MessageFlags::default(),
        labels: vec![],
        snippet: String::new(),
        size: 1024,
        has_attachments: false,
    }
}

async fn seed_account(conn: &TursoConn) -> (AccountId, Folder) {
    let acct = Account {
        id: AccountId("acct".into()),
        kind: BackendKind::ImapSmtp,
        display_name: "x".into(),
        email_address: "x@example.com".into(),
        created_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
    };
    capytain_storage::repos::accounts::insert(conn, &acct)
        .await
        .unwrap();
    let folder = Folder {
        id: FolderId("INBOX".into()),
        account_id: acct.id.clone(),
        name: "Inbox".into(),
        path: "INBOX".into(),
        role: Some(FolderRole::Inbox),
        unread_count: 0,
        total_count: 0,
        parent: None,
    };
    (acct.id, folder)
}

// ---------- Tests ----------

#[test]
fn sync_folder_inserts_new_headers_and_persists_cursor() {
    rt().block_on(async {
        let conn = TursoConn::in_memory().await.unwrap();
        run_migrations(&conn).await.unwrap();
        let (acct_id, folder) = seed_account(&conn).await;

        let h1 = header("m1", &acct_id, &folder.id, "first");
        let h2 = header("m2", &acct_id, &folder.id, "second");
        let backend = StubBackend {
            folders: vec![folder.clone()],
            responses: Mutex::new(vec![MessageList {
                messages: vec![h1.clone(), h2.clone()],
                flag_updates: vec![],
                new_state: SyncState {
                    folder_id: folder.id.clone(),
                    backend_state: "{\"uidvalidity\":1,\"highestmodseq\":0,\"uidnext\":3}".into(),
                },
                removed: vec![],
            }]),
            raw_bodies: Default::default(),
            failing_ids: Default::default(),
        };

        let report = sync_folder(&conn, &backend, None, &folder, None)
            .await
            .unwrap();
        assert_eq!(report.added, 2);
        assert_eq!(report.updated, 0);
        assert_eq!(report.removed, 0);

        // Headers landed in storage.
        let stored = messages_repo::get(&conn, &h1.id).await.unwrap();
        assert_eq!(stored.subject, "first");

        // Cursor persisted.
        let cursor = sync_states_repo::get(&conn, &folder.id)
            .await
            .unwrap()
            .expect("cursor");
        assert!(cursor.backend_state.contains("uidnext"));

        // Folder upserted.
        assert!(folders_repo::find(&conn, &folder.id)
            .await
            .unwrap()
            .is_some());
    });
}

#[test]
fn sync_folder_updates_existing_headers() {
    rt().block_on(async {
        let conn = TursoConn::in_memory().await.unwrap();
        run_migrations(&conn).await.unwrap();
        let (acct_id, folder) = seed_account(&conn).await;

        let mut h1 = header("m1", &acct_id, &folder.id, "first");
        let backend = StubBackend {
            folders: vec![folder.clone()],
            responses: Mutex::new(vec![
                MessageList {
                    messages: vec![h1.clone()],
                    flag_updates: vec![],
                    new_state: SyncState {
                        folder_id: folder.id.clone(),
                        backend_state: "{\"uidvalidity\":1,\"highestmodseq\":0,\"uidnext\":2}"
                            .into(),
                    },
                    removed: vec![],
                },
                // Second cycle: same UID, flag flipped to seen.
                {
                    h1.flags.seen = true;
                    MessageList {
                        messages: vec![h1.clone()],
                        flag_updates: vec![],
                        new_state: SyncState {
                            folder_id: folder.id.clone(),
                            backend_state: "{\"uidvalidity\":1,\"highestmodseq\":1,\"uidnext\":2}"
                                .into(),
                        },
                        removed: vec![],
                    }
                },
            ]),
            raw_bodies: Default::default(),
            failing_ids: Default::default(),
        };

        let r1 = sync_folder(&conn, &backend, None, &folder, None)
            .await
            .unwrap();
        assert_eq!(r1.added, 1);
        assert_eq!(r1.updated, 0);

        let r2 = sync_folder(&conn, &backend, None, &folder, None)
            .await
            .unwrap();
        assert_eq!(r2.added, 0);
        assert_eq!(r2.updated, 1);

        let stored = messages_repo::get(&conn, &h1.id).await.unwrap();
        assert!(stored.flags.seen);
    });
}

#[test]
fn sync_folder_fetches_bodies_for_new_messages() {
    rt().block_on(async {
        let conn = TursoConn::in_memory().await.unwrap();
        run_migrations(&conn).await.unwrap();
        let (acct_id, folder) = seed_account(&conn).await;

        let h1 = header("m1", &acct_id, &folder.id, "first");
        let h2 = header("m2", &acct_id, &folder.id, "second");

        let mut bodies = std::collections::HashMap::new();
        bodies.insert(
            "m1".to_string(),
            b"From: a\r\nSubject: first\r\n\r\nbody-1\r\n".to_vec(),
        );
        bodies.insert(
            "m2".to_string(),
            b"From: b\r\nSubject: second\r\n\r\nbody-2\r\n".to_vec(),
        );

        let backend = StubBackend {
            folders: vec![folder.clone()],
            responses: Mutex::new(vec![MessageList {
                messages: vec![h1.clone(), h2.clone()],
                flag_updates: vec![],
                new_state: SyncState {
                    folder_id: folder.id.clone(),
                    backend_state: "{\"uidvalidity\":1,\"highestmodseq\":0,\"uidnext\":3}".into(),
                },
                removed: vec![],
            }]),
            raw_bodies: bodies,
            failing_ids: Default::default(),
        };

        let tmp = scratch_dir();
        let blobs = BlobStore::new(&tmp);

        let report = sync_folder(&conn, &backend, Some(&blobs), &folder, None)
            .await
            .unwrap();
        assert_eq!(report.added, 2);
        assert_eq!(report.bodies_fetched, 2);
        assert_eq!(report.bodies_failed, 0);

        // body_path now non-null for both messages.
        let p1 = messages_repo::body_path(&conn, &h1.id).await.unwrap();
        assert!(p1.is_some(), "body_path missing for m1");

        // BlobStore::get returns the same bytes we handed the stub.
        let back = blobs
            .get(&acct_id, &folder.id, &h1.id)
            .await
            .expect("blob present");
        assert_eq!(back, b"From: a\r\nSubject: first\r\n\r\nbody-1\r\n");
    });
}

#[test]
fn sync_folder_logs_and_skips_failed_body_fetches() {
    rt().block_on(async {
        let conn = TursoConn::in_memory().await.unwrap();
        run_migrations(&conn).await.unwrap();
        let (acct_id, folder) = seed_account(&conn).await;

        let h_ok = header("ok", &acct_id, &folder.id, "fine");
        let h_bad = header("bad", &acct_id, &folder.id, "broken");

        let mut bodies = std::collections::HashMap::new();
        bodies.insert("ok".to_string(), b"raw ok\r\n".to_vec());
        // h_bad has no body in the map; even if it did, failing_ids
        // forces a Protocol error.
        let mut failing = std::collections::HashSet::new();
        failing.insert("bad".to_string());

        let backend = StubBackend {
            folders: vec![folder.clone()],
            responses: Mutex::new(vec![MessageList {
                messages: vec![h_ok.clone(), h_bad.clone()],
                flag_updates: vec![],
                new_state: SyncState {
                    folder_id: folder.id.clone(),
                    backend_state: "{\"uidvalidity\":1,\"highestmodseq\":0,\"uidnext\":3}".into(),
                },
                removed: vec![],
            }]),
            raw_bodies: bodies,
            failing_ids: failing,
        };

        let tmp = scratch_dir();
        let blobs = BlobStore::new(&tmp);

        let report = sync_folder(&conn, &backend, Some(&blobs), &folder, None)
            .await
            .unwrap();
        assert_eq!(report.added, 2);
        assert_eq!(report.bodies_fetched, 1);
        assert_eq!(report.bodies_failed, 1);

        // The good message has a body_path; the bad one does not.
        assert!(messages_repo::body_path(&conn, &h_ok.id)
            .await
            .unwrap()
            .is_some());
        assert!(messages_repo::body_path(&conn, &h_bad.id)
            .await
            .unwrap()
            .is_none());
    });
}

#[test]
fn sync_folder_applies_flag_updates_via_update_flags() {
    rt().block_on(async {
        let conn = TursoConn::in_memory().await.unwrap();
        run_migrations(&conn).await.unwrap();
        let (acct_id, folder) = seed_account(&conn).await;

        // Cycle 1: insert m1 with default flags.
        let h1 = header("m1", &acct_id, &folder.id, "first");
        // Cycle 2: m1 disappears from `messages` (server says no new
        // appends), but its flags moved per CHANGEDSINCE — `flag_updates`
        // carries the delta.
        let new_flags = MessageFlags {
            seen: true,
            flagged: true,
            ..Default::default()
        };

        let backend = StubBackend {
            folders: vec![folder.clone()],
            responses: Mutex::new(vec![
                MessageList {
                    messages: vec![h1.clone()],
                    flag_updates: vec![],
                    new_state: SyncState {
                        folder_id: folder.id.clone(),
                        backend_state: "{\"uidvalidity\":1,\"highestmodseq\":1,\"uidnext\":2}"
                            .into(),
                    },
                    removed: vec![],
                },
                MessageList {
                    messages: vec![],
                    flag_updates: vec![(h1.id.clone(), new_flags.clone())],
                    new_state: SyncState {
                        folder_id: folder.id.clone(),
                        backend_state: "{\"uidvalidity\":1,\"highestmodseq\":2,\"uidnext\":2}"
                            .into(),
                    },
                    removed: vec![],
                },
            ]),
            raw_bodies: Default::default(),
            failing_ids: Default::default(),
        };

        let r1 = sync_folder(&conn, &backend, None, &folder, None)
            .await
            .unwrap();
        assert_eq!(r1.added, 1);
        assert_eq!(r1.flag_updates, 0);

        // Pre-cycle-2: stored row has default flags.
        let pre = messages_repo::get(&conn, &h1.id).await.unwrap();
        assert!(!pre.flags.seen);

        let r2 = sync_folder(&conn, &backend, None, &folder, None)
            .await
            .unwrap();
        assert_eq!(r2.added, 0);
        assert_eq!(r2.updated, 0);
        assert_eq!(r2.flag_updates, 1);

        // Post-cycle-2: stored row picked up the flag delta.
        let post = messages_repo::get(&conn, &h1.id).await.unwrap();
        assert!(post.flags.seen);
        assert!(post.flags.flagged);
    });
}

#[test]
fn sync_folder_skips_flag_updates_for_unknown_messages() {
    rt().block_on(async {
        let conn = TursoConn::in_memory().await.unwrap();
        run_migrations(&conn).await.unwrap();
        let (_acct_id, folder) = seed_account(&conn).await;

        // Cache has nothing in it. CHANGEDSINCE pass surfaces an
        // update for a UID we never inserted (earlier bounded sync
        // didn't pull this far back). `sync_folder` should log + skip,
        // not propagate StorageError::NotFound.
        let stranger_flags = MessageFlags {
            seen: true,
            ..Default::default()
        };
        let backend = StubBackend {
            folders: vec![folder.clone()],
            responses: Mutex::new(vec![MessageList {
                messages: vec![],
                flag_updates: vec![(MessageId("never-inserted".into()), stranger_flags)],
                new_state: SyncState {
                    folder_id: folder.id.clone(),
                    backend_state: "{\"uidvalidity\":1,\"highestmodseq\":2,\"uidnext\":1}".into(),
                },
                removed: vec![],
            }]),
            raw_bodies: Default::default(),
            failing_ids: Default::default(),
        };

        let r = sync_folder(&conn, &backend, None, &folder, None)
            .await
            .unwrap();
        assert_eq!(r.added, 0);
        assert_eq!(r.flag_updates, 0); // skipped, not counted
    });
}

/// Local tempdir helper — `capytain-storage` rolls its own to avoid a
/// `tempfile` dev-dep, so this crate does the same.
fn scratch_dir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("capytain-sync-test-{}-{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn sync_folder_reports_removed_count() {
    rt().block_on(async {
        let conn = TursoConn::in_memory().await.unwrap();
        run_migrations(&conn).await.unwrap();
        let (_acct_id, folder) = seed_account(&conn).await;

        let backend = StubBackend {
            folders: vec![folder.clone()],
            responses: Mutex::new(vec![MessageList {
                messages: vec![],
                flag_updates: vec![],
                new_state: SyncState {
                    folder_id: folder.id.clone(),
                    backend_state: "{\"uidvalidity\":1,\"highestmodseq\":0,\"uidnext\":1}".into(),
                },
                removed: vec![
                    MessageId("m_gone_1".into()),
                    MessageId("m_gone_2".into()),
                    MessageId("m_gone_3".into()),
                ],
            }]),
            raw_bodies: Default::default(),
            failing_ids: Default::default(),
        };

        let report = sync_folder(&conn, &backend, None, &folder, None)
            .await
            .unwrap();
        assert_eq!(report.removed, 3);
    });
}
