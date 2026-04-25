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
    run_migrations, TursoConn,
};

use capytain_sync::sync_folder;

// ---------- Stub backend ----------

/// Minimal `MailBackend` that returns a scripted sequence of
/// `MessageList` responses. Each call to `list_messages` consumes one
/// from the front of the queue.
struct StubBackend {
    folders: Vec<Folder>,
    responses: Mutex<Vec<MessageList>>,
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
        unimplemented!("body fetch lands in PR 2")
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
                new_state: SyncState {
                    folder_id: folder.id.clone(),
                    backend_state: "{\"uidvalidity\":1,\"highestmodseq\":0,\"uidnext\":3}".into(),
                },
                removed: vec![],
            }]),
        };

        let report = sync_folder(&conn, &backend, &folder, None).await.unwrap();
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
                        new_state: SyncState {
                            folder_id: folder.id.clone(),
                            backend_state: "{\"uidvalidity\":1,\"highestmodseq\":1,\"uidnext\":2}"
                                .into(),
                        },
                        removed: vec![],
                    }
                },
            ]),
        };

        let r1 = sync_folder(&conn, &backend, &folder, None).await.unwrap();
        assert_eq!(r1.added, 1);
        assert_eq!(r1.updated, 0);

        let r2 = sync_folder(&conn, &backend, &folder, None).await.unwrap();
        assert_eq!(r2.added, 0);
        assert_eq!(r2.updated, 1);

        let stored = messages_repo::get(&conn, &h1.id).await.unwrap();
        assert!(stored.flags.seen);
    });
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
        };

        let report = sync_folder(&conn, &backend, &folder, None).await.unwrap();
        assert_eq!(report.removed, 3);
    });
}
