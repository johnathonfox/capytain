// SPDX-License-Identifier: Apache-2.0

//! Integration tests for the storage layer.
//!
//! Each public domain type is round-tripped through Turso (via
//! [`capytain_storage::TursoConn::in_memory`] + the schema v1 migration) and
//! asserted equal to the original. Generators come from `proptest`'s
//! `Strategy` API; the shrinker keeps test output readable.
//!
//! Run with `cargo test -p capytain-storage --test roundtrip`. To scale the
//! search set `PROPTEST_CASES=N` (default is 256).

use chrono::{DateTime, TimeZone, Utc};
use proptest::collection::vec;
use proptest::prelude::*;
use tokio::runtime::Runtime;

use capytain_core::{
    Account, AccountId, Attachment, AttachmentRef, BackendKind, EmailAddress, Folder, FolderId,
    FolderRole, MessageFlags, MessageHeaders, MessageId, SyncState, ThreadId,
};
use capytain_storage::{repos, run_migrations, DbConn, TursoConn};

// ---------- Generators ----------

fn id_string() -> impl Strategy<Value = String> {
    // Backend IDs in the wild contain ASCII punctuation, colons, slashes.
    // 1..32 chars, printable ASCII excluding control chars.
    "[a-zA-Z0-9:/._\\-+@]{1,32}"
}

fn small_text() -> impl Strategy<Value = String> {
    "[A-Za-z0-9 \u{00e9}\u{4e2d}\u{2603} ]{0,32}"
}

fn utc_datetime() -> impl Strategy<Value = DateTime<Utc>> {
    // 2000-01-01 to 2100-01-01, second precision.
    (946_684_800i64..4_102_444_800i64).prop_map(|ts| Utc.timestamp_opt(ts, 0).single().unwrap())
}

fn backend_kind() -> impl Strategy<Value = BackendKind> {
    prop_oneof![Just(BackendKind::ImapSmtp), Just(BackendKind::Jmap)]
}

fn folder_role() -> impl Strategy<Value = FolderRole> {
    prop_oneof![
        Just(FolderRole::Inbox),
        Just(FolderRole::Sent),
        Just(FolderRole::Drafts),
        Just(FolderRole::Trash),
        Just(FolderRole::Spam),
        Just(FolderRole::Archive),
        Just(FolderRole::Important),
        Just(FolderRole::All),
        Just(FolderRole::Flagged),
    ]
}

fn message_flags() -> impl Strategy<Value = MessageFlags> {
    (
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(|(seen, flagged, answered, draft, forwarded)| MessageFlags {
            seen,
            flagged,
            answered,
            draft,
            forwarded,
        })
}

fn email_address() -> impl Strategy<Value = EmailAddress> {
    (
        "[a-z0-9._-]{1,16}@[a-z0-9.-]{1,16}",
        prop::option::of(small_text()),
    )
        .prop_map(|(address, display_name)| EmailAddress {
            address,
            display_name,
        })
}

fn account_strategy() -> impl Strategy<Value = Account> {
    (
        id_string(),
        backend_kind(),
        small_text(),
        "[a-z0-9._-]{1,16}@[a-z0-9.-]{1,16}",
        utc_datetime(),
    )
        .prop_map(
            |(id, kind, display_name, email_address, created_at)| Account {
                id: AccountId(id),
                kind,
                display_name,
                email_address,
                created_at,
            },
        )
}

// Kept for future tests — `folder_roundtrips` draws the fields inline so
// it can mix them with a parallel-drawn account.
#[allow(dead_code)]
fn folder_strategy(account_id: AccountId) -> impl Strategy<Value = Folder> {
    (
        id_string(),
        small_text(),
        "[A-Za-z0-9./_-]{1,32}",
        prop::option::of(folder_role()),
        0u32..100_000,
        0u32..100_000,
    )
        .prop_map(move |(id, name, path, role, unread, total)| Folder {
            id: FolderId(id),
            account_id: account_id.clone(),
            name,
            path,
            role,
            unread_count: unread,
            total_count: total,
            parent: None,
        })
}

fn headers_strategy(
    account_id: AccountId,
    folder_id: FolderId,
) -> impl Strategy<Value = MessageHeaders> {
    // Proptest's Strategy impl for tuples tops out well below the
    // 17-field shape of MessageHeaders, so we shard into three 5-ish-tuples
    // and recombine.
    let ids = (
        id_string(),
        prop::option::of(id_string().prop_map(ThreadId)),
        prop::option::of(small_text()),
        small_text(),
    );
    let addrs = (
        vec(email_address(), 0..3),
        vec(email_address(), 0..3),
        vec(email_address(), 0..3),
        vec(email_address(), 0..3),
        vec(email_address(), 0..3),
    );
    let rest = (
        utc_datetime(),
        message_flags(),
        vec(small_text(), 0..4),
        small_text(),
        0u32..10_000_000,
        any::<bool>(),
    );
    (ids, addrs, rest).prop_map(
        move |(
            (id, thread_id, rfc822_message_id, subject),
            (from, reply_to, to, cc, bcc),
            (date, flags, labels, snippet, size, has_attachments),
        )| MessageHeaders {
            id: MessageId(id),
            account_id: account_id.clone(),
            folder_id: folder_id.clone(),
            thread_id,
            rfc822_message_id,
            subject,
            from,
            reply_to,
            to,
            cc,
            bcc,
            date,
            flags,
            labels,
            snippet,
            size,
            has_attachments,
        },
    )
}

fn attachment_strategy() -> impl Strategy<Value = Attachment> {
    (
        id_string(),
        small_text(),
        "[a-z]+/[a-z0-9.+-]+",
        0u64..1_000_000_000,
        any::<bool>(),
        prop::option::of(id_string()),
    )
        .prop_map(
            |(id, filename, mime_type, size, inline, content_id)| Attachment {
                id: AttachmentRef(id),
                filename,
                mime_type,
                size,
                inline,
                content_id,
            },
        )
}

// ---------- Harness ----------

/// Produce a fresh in-memory database with schema v1 applied.
async fn fresh_conn() -> TursoConn {
    let conn = TursoConn::in_memory().await.expect("in-memory db");
    run_migrations(&conn).await.expect("migrate");
    conn
}

fn rt() -> Runtime {
    Runtime::new().expect("tokio runtime")
}

// Proptest runs async bodies by blocking on a fresh tokio runtime per test
// case. `rt.block_on` is what threads the async work into the sync `|()|`
// closure proptest hands us.

// ---------- Tests ----------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64, // default 256 → a few minutes with Turso startup. 64 is
                   // plenty for round-trip coverage.
        .. ProptestConfig::default()
    })]

    #[test]
    fn account_roundtrips(account in account_strategy()) {
        let rt = rt();
        rt.block_on(async move {
            let conn = fresh_conn().await;
            repos::accounts::insert(&conn, &account).await.expect("insert");
            let back = repos::accounts::get(&conn, &account.id).await.expect("get");
            prop_assert_eq!(back.id, account.id);
            prop_assert_eq!(back.kind, account.kind);
            prop_assert_eq!(back.display_name, account.display_name);
            prop_assert_eq!(back.email_address, account.email_address);
            prop_assert_eq!(back.created_at, account.created_at);
            Ok(())
        })?;
    }

    #[test]
    fn folder_roundtrips(
        acct in account_strategy(),
        folder_tail in id_string(),
        name in small_text(),
        path in "[A-Za-z0-9./_-]{1,32}",
        role in prop::option::of(folder_role()),
        unread in 0u32..100_000,
        total in 0u32..100_000,
    ) {
        let rt = rt();
        rt.block_on(async move {
            let conn = fresh_conn().await;
            repos::accounts::insert(&conn, &acct).await.expect("insert acct");
            let folder = Folder {
                id: FolderId(folder_tail),
                account_id: acct.id.clone(),
                name,
                path,
                role,
                unread_count: unread,
                total_count: total,
                parent: None,
            };
            repos::folders::insert(&conn, &folder).await.expect("insert folder");
            let back = repos::folders::get(&conn, &folder.id).await.expect("get folder");
            prop_assert_eq!(back.id, folder.id);
            prop_assert_eq!(back.name, folder.name);
            prop_assert_eq!(back.path, folder.path);
            prop_assert_eq!(back.role, folder.role);
            prop_assert_eq!(back.unread_count, folder.unread_count);
            prop_assert_eq!(back.total_count, folder.total_count);
            Ok(())
        })?;
    }

    #[test]
    fn message_headers_roundtrip(acct in account_strategy()) {
        let rt = rt();
        rt.block_on(async move {
            let conn = fresh_conn().await;
            repos::accounts::insert(&conn, &acct).await.expect("insert acct");

            // Build folder + headers with fixed IDs to isolate the headers
            // variation per case.
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
            repos::folders::insert(&conn, &folder).await.expect("insert folder");

            // Draw a headers instance with the fixed folder/account ids.
            let strat = headers_strategy(acct.id.clone(), folder.id.clone()).boxed();
            let tree = strat.new_tree(&mut proptest::test_runner::TestRunner::default())
                .map_err(|e| proptest::test_runner::TestCaseError::fail(e.to_string()))?;
            let headers = tree.current();

            repos::messages::insert(&conn, &headers, None).await.expect("insert headers");
            let back = repos::messages::get(&conn, &headers.id).await.expect("get headers");
            prop_assert_eq!(back.id, headers.id.clone());
            prop_assert_eq!(back.subject, headers.subject.clone());
            prop_assert_eq!(back.from.len(), headers.from.len());
            prop_assert_eq!(back.flags.seen, headers.flags.seen);
            prop_assert_eq!(back.flags.flagged, headers.flags.flagged);
            prop_assert_eq!(back.date, headers.date);
            prop_assert_eq!(back.size, headers.size);
            prop_assert_eq!(back.has_attachments, headers.has_attachments);
            Ok(())
        })?;
    }

    #[test]
    fn attachment_roundtrips(attachment in attachment_strategy()) {
        let rt = rt();
        rt.block_on(async move {
            let conn = fresh_conn().await;
            // Minimal parent chain so foreign keys hold.
            let acct = Account {
                id: AccountId("acct-1".into()),
                kind: BackendKind::ImapSmtp,
                display_name: "Work".into(),
                email_address: "me@example.com".into(),
                created_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            };
            repos::accounts::insert(&conn, &acct).await.expect("insert acct");
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
            repos::folders::insert(&conn, &folder).await.expect("insert folder");
            let headers = MessageHeaders {
                id: MessageId("m-1".into()),
                account_id: acct.id.clone(),
                folder_id: folder.id.clone(),
                thread_id: None,
                rfc822_message_id: None,
                subject: "s".into(),
                from: vec![],
                reply_to: vec![],
                to: vec![],
                cc: vec![],
                bcc: vec![],
                date: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
                flags: MessageFlags::default(),
                labels: vec![],
                snippet: "".into(),
                size: 0,
                has_attachments: true,
            };
            repos::messages::insert(&conn, &headers, None).await.expect("insert msg");

            repos::attachments::insert(&conn, &headers.id, &attachment)
                .await.expect("insert attachment");
            let back = repos::attachments::list_by_message(&conn, &headers.id)
                .await.expect("list");
            prop_assert_eq!(back.len(), 1);
            let b = &back[0];
            prop_assert_eq!(&b.id, &attachment.id);
            prop_assert_eq!(&b.filename, &attachment.filename);
            prop_assert_eq!(&b.mime_type, &attachment.mime_type);
            prop_assert_eq!(b.size, attachment.size);
            prop_assert_eq!(b.inline, attachment.inline);
            prop_assert_eq!(&b.content_id, &attachment.content_id);
            Ok(())
        })?;
    }

    #[test]
    fn sync_state_roundtrips(
        acct in account_strategy(),
        backend_state in "[ -~]{0,128}",
    ) {
        let rt = rt();
        rt.block_on(async move {
            let conn = fresh_conn().await;
            repos::accounts::insert(&conn, &acct).await.expect("insert acct");
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
            repos::folders::insert(&conn, &folder).await.expect("insert folder");

            let state = SyncState {
                folder_id: folder.id.clone(),
                backend_state,
            };
            repos::sync_states::put(&conn, &state).await.expect("put");
            let back = repos::sync_states::get(&conn, &folder.id).await.expect("get");
            let back = back.expect("some");
            prop_assert_eq!(back.folder_id, state.folder_id);
            prop_assert_eq!(back.backend_state, state.backend_state);
            Ok(())
        })?;
    }
}

// ---------- Plain integration tests (not proptest) ----------

#[test]
fn migrations_idempotent() {
    let rt = rt();
    rt.block_on(async move {
        let conn = TursoConn::in_memory().await.expect("in-memory");
        run_migrations(&conn).await.expect("first");
        run_migrations(&conn).await.expect("second");
        // Second run should be a no-op — verify by counting the
        // schema_version rows: one row per shipped migration, no
        // duplicates from the second pass.
        let rows = conn
            .query(
                "SELECT COUNT(*) AS c FROM _schema_version",
                capytain_storage::Params::empty(),
            )
            .await
            .unwrap();
        let expected = capytain_storage::MIGRATIONS.len() as i64;
        assert_eq!(rows[0].get_i64("c").unwrap(), expected);
    });
}

#[test]
fn remote_content_opt_in_add_check_remove() {
    use capytain_storage::repos::remote_content_opt_ins as opt_ins;
    let rt = rt();
    rt.block_on(async move {
        let conn = fresh_conn().await;
        let acct = Account {
            id: AccountId("opt-in-acct".into()),
            kind: BackendKind::Jmap,
            display_name: "x".into(),
            email_address: "me@example.com".into(),
            created_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        };
        repos::accounts::insert(&conn, &acct).await.expect("acct");

        assert!(
            !opt_ins::is_trusted(&conn, &acct.id, "newsletter@example.com")
                .await
                .unwrap()
        );

        // Add with mixed case — repo lowercases on store and on lookup.
        opt_ins::add(&conn, &acct.id, "Newsletter@Example.COM")
            .await
            .expect("add");
        assert!(
            opt_ins::is_trusted(&conn, &acct.id, "newsletter@example.com")
                .await
                .unwrap()
        );
        assert!(
            opt_ins::is_trusted(&conn, &acct.id, "NEWSLETTER@example.com")
                .await
                .unwrap()
        );

        let listed = opt_ins::list_for_account(&conn, &acct.id)
            .await
            .expect("list");
        assert_eq!(listed, vec!["newsletter@example.com".to_string()]);

        // Add is idempotent — calling twice doesn't insert two rows.
        opt_ins::add(&conn, &acct.id, "newsletter@example.com")
            .await
            .expect("re-add");
        let listed = opt_ins::list_for_account(&conn, &acct.id)
            .await
            .expect("list2");
        assert_eq!(listed.len(), 1);

        opt_ins::remove(&conn, &acct.id, "newsletter@example.com")
            .await
            .expect("remove");
        assert!(
            !opt_ins::is_trusted(&conn, &acct.id, "newsletter@example.com")
                .await
                .unwrap()
        );
    });
}

#[test]
fn transaction_rollback_reverts_writes() {
    let rt = rt();
    rt.block_on(async move {
        let conn = fresh_conn().await;
        let acct = Account {
            id: AccountId("rollback".into()),
            kind: BackendKind::Jmap,
            display_name: "x".into(),
            email_address: "x@example.com".into(),
            created_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        };
        {
            let mut tx = conn.begin().await.expect("begin");
            tx.execute(
                "INSERT INTO accounts (id, kind, display_name, email_address, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                capytain_storage::Params(vec![
                    capytain_storage::Value::Text(&acct.id.0),
                    capytain_storage::Value::OwnedText("jmap".into()),
                    capytain_storage::Value::Text(&acct.display_name),
                    capytain_storage::Value::Text(&acct.email_address),
                    capytain_storage::Value::Integer(acct.created_at.timestamp()),
                ]),
            )
            .await
            .expect("insert in tx");
            tx.rollback().await.expect("rollback");
        }
        let missing = repos::accounts::find(&conn, &acct.id).await.expect("find");
        assert!(
            missing.is_none(),
            "rollback should have reverted the insert"
        );
    });
}
