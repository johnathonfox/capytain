// SPDX-License-Identifier: Apache-2.0

//! Integration test: every public domain type round-trips through
//! `serde_json`.
//!
//! This lives in `tests/` per the convention in `CONTRIBUTING.md` and
//! `PHASE_0.md` Week 1 Day 4. The goal is twofold:
//!
//! 1. Lock the on-the-wire JSON shape, which is the shape the IPC layer
//!    (see `COMMANDS.md`) will hand to the Dioxus UI.
//! 2. Serve as the template for future integration tests — one file, one
//!    scenario, fixtures under `tests/fixtures/` when needed.

use chrono::{TimeZone, Utc};
use serde::de::DeserializeOwned;
use serde::Serialize;

use capytain_core::{
    Account, AccountId, Attachment, AttachmentRef, BackendKind, DraftId, EmailAddress, Folder,
    FolderId, FolderRole, MessageBody, MessageFlags, MessageHeaders, MessageId, SyncState,
    ThreadId,
};

/// Serialize a value to JSON and back again; assert the round-trip is
/// lossless under serde_json::Value equality.
fn assert_roundtrips<T>(value: &T)
where
    T: Serialize + DeserializeOwned,
{
    let json = serde_json::to_string(value).expect("serialize");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse JSON");
    let back: T = serde_json::from_str(&json).expect("deserialize");
    let back_json = serde_json::to_value(&back).expect("reserialize");
    assert_eq!(parsed, back_json, "JSON changed across a serde round-trip");
}

#[test]
fn ids_roundtrip() {
    assert_roundtrips(&AccountId("gmail:foo@example.com".into()));
    assert_roundtrips(&FolderId("INBOX".into()));
    assert_roundtrips(&MessageId("1712345:42".into()));
    assert_roundtrips(&ThreadId("thrd-abc".into()));
    assert_roundtrips(&DraftId("draft-1".into()));
    assert_roundtrips(&AttachmentRef("1.2".into()));
}

#[test]
fn account_roundtrips() {
    let account = Account {
        id: AccountId("acct-1".into()),
        kind: BackendKind::ImapSmtp,
        display_name: "Work".into(),
        email_address: "me@example.com".into(),
        created_at: Utc.with_ymd_and_hms(2026, 4, 18, 12, 0, 0).unwrap(),
    };
    assert_roundtrips(&account);
}

#[test]
fn folder_roundtrips_with_and_without_role() {
    let inbox = Folder {
        id: FolderId("INBOX".into()),
        account_id: AccountId("acct-1".into()),
        name: "Inbox".into(),
        path: "INBOX".into(),
        role: Some(FolderRole::Inbox),
        unread_count: 3,
        total_count: 100,
        parent: None,
    };
    assert_roundtrips(&inbox);

    let unnamed = Folder {
        id: FolderId("Junk Drawer".into()),
        account_id: AccountId("acct-1".into()),
        name: "Junk Drawer".into(),
        path: "Junk Drawer".into(),
        role: None,
        unread_count: 0,
        total_count: 0,
        parent: Some(FolderId("INBOX".into())),
    };
    assert_roundtrips(&unnamed);
}

#[test]
fn message_headers_and_body_roundtrip() {
    let headers = MessageHeaders {
        id: MessageId("1712345:42".into()),
        account_id: AccountId("acct-1".into()),
        folder_id: FolderId("INBOX".into()),
        thread_id: Some(ThreadId("thrd-abc".into())),
        rfc822_message_id: Some("<abc@example.com>".into()),
        subject: "Hello".into(),
        from: vec![EmailAddress {
            address: "jane@example.com".into(),
            display_name: Some("Jane Doe".into()),
        }],
        reply_to: vec![],
        to: vec![EmailAddress {
            address: "me@example.com".into(),
            display_name: None,
        }],
        cc: vec![],
        bcc: vec![],
        date: Utc.with_ymd_and_hms(2026, 4, 18, 9, 30, 0).unwrap(),
        flags: MessageFlags {
            seen: true,
            ..Default::default()
        },
        labels: vec!["Important".into()],
        snippet: "Just checking in…".into(),
        size: 2048,
        has_attachments: true,
        in_reply_to: Some("<prev@example.com>".into()),
        references: vec!["<root@example.com>".into(), "<prev@example.com>".into()],
    };
    assert_roundtrips(&headers);

    let body = MessageBody {
        headers: headers.clone(),
        body_html: Some("<p>Just checking in\u{2026}</p>".into()),
        body_text: Some("Just checking in\u{2026}".into()),
        attachments: vec![Attachment {
            id: AttachmentRef("2".into()),
            filename: "receipt.pdf".into(),
            mime_type: "application/pdf".into(),
            size: 12_345,
            inline: false,
            content_id: None,
        }],
        in_reply_to: Some("<prev@example.com>".into()),
        references: vec!["<older@example.com>".into(), "<prev@example.com>".into()],
    };
    assert_roundtrips(&body);
}

#[test]
fn sync_state_roundtrips() {
    let state = SyncState {
        folder_id: FolderId("INBOX".into()),
        backend_state: "uidvalidity=1712345;highestmodseq=7890;uidnext=43".into(),
    };
    assert_roundtrips(&state);
}
