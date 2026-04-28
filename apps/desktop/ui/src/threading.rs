// SPDX-License-Identifier: Apache-2.0

//! Client-side adjacent-thread grouping for the message list.
//!
//! `messages_list` returns `MessageHeaders` in `DateDesc` order with a
//! per-message `Option<ThreadId>`. The visible message list collapses
//! consecutive same-thread headers into a single row with a count
//! badge — Gmail's default behavior. We deliberately group **only
//! adjacent** rows (same thread, contiguous in the page) rather than
//! reaching across the page. The full thread can still span non-
//! adjacent positions when activity is bursty across multiple threads,
//! and PR-H2's stacked-card thread reader shows the holistic view.
//! Adjacent-only keeps the message list a pure transformation of the
//! page payload, no extra IPC.
//!
//! Pure logic, no Dioxus or wasm dependencies — kept in its own module
//! so it stays reachable from `cargo test` on the host (see `main.rs`'s
//! `cfg(all(test, not(target_arch = "wasm32")))` mounting).

use qsl_ipc::{MessageHeaders, ThreadId};

/// One row in the rendered message list. `Single` is the historical
/// shape (one row, one message); `Thread` is two-or-more consecutive
/// messages sharing a `thread_id` rolled up into one parent row plus
/// the full member list (head first, oldest last) for inline
/// expansion.
#[derive(Debug, Clone)]
pub enum MessageListItem {
    Single(MessageHeaders),
    Thread {
        /// Newest message in the run; drives the collapsed row's
        /// metadata (sender, subject, date).
        head: MessageHeaders,
        /// Every message in the run, in the same order they appeared
        /// in the page (DateDesc → newest first). `members[0] == head`
        /// — kept inline so callers iterating `members` to render the
        /// expanded view don't have to special-case the head.
        members: Vec<MessageHeaders>,
    },
}

/// Walk a DateDesc page of headers and roll up adjacent same-thread
/// runs. Singletons (`thread_id == None`) are never grouped, even with
/// each other — `None` means "the threading pass hasn't classified
/// this message yet" or "this message has no In-Reply-To/References,"
/// and either way it's not safe to merge two `None`s into a thread.
pub fn group_by_thread(messages: Vec<MessageHeaders>) -> Vec<MessageListItem> {
    let mut out: Vec<MessageListItem> = Vec::with_capacity(messages.len());
    let mut buf: Vec<MessageHeaders> = Vec::new();
    let mut current: Option<ThreadId> = None;

    for m in messages {
        // Extending the current run requires both sides to have the
        // same `Some(tid)`; `None` always breaks the run.
        let extends = m.thread_id.is_some() && m.thread_id == current;
        if !extends {
            flush_group(&mut out, &mut buf);
            current = m.thread_id.clone();
        }
        buf.push(m);
    }
    flush_group(&mut out, &mut buf);
    out
}

fn flush_group(out: &mut Vec<MessageListItem>, buf: &mut Vec<MessageHeaders>) {
    match buf.len() {
        0 => {}
        1 => out.push(MessageListItem::Single(buf.remove(0))),
        _ => {
            let group: Vec<MessageHeaders> = std::mem::take(buf);
            let head = group[0].clone();
            out.push(MessageListItem::Thread {
                head,
                members: group,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use qsl_ipc::{AccountId, EmailAddress, FolderId, MessageFlags, MessageId};

    fn header(id: &str, thread: Option<&str>, secs_since_epoch: i64) -> MessageHeaders {
        MessageHeaders {
            id: MessageId(id.into()),
            account_id: AccountId("acct".into()),
            folder_id: FolderId("INBOX".into()),
            thread_id: thread.map(|t| ThreadId(t.into())),
            rfc822_message_id: None,
            in_reply_to: None,
            references: vec![],
            from: vec![EmailAddress {
                address: "alice@example.com".into(),
                display_name: Some("Alice".into()),
            }],
            to: vec![],
            cc: vec![],
            bcc: vec![],
            reply_to: vec![],
            subject: format!("Subject {id}"),
            snippet: String::new(),
            date: Utc.timestamp_opt(secs_since_epoch, 0).unwrap(),
            flags: MessageFlags::default(),
            labels: vec![],
            size: 0,
            has_attachments: false,
        }
    }

    #[test]
    fn empty_input_yields_empty_output() {
        let groups = group_by_thread(vec![]);
        assert!(groups.is_empty());
    }

    #[test]
    fn singletons_stay_single() {
        let msgs = vec![
            header("m1", None, 100),
            header("m2", Some("t2"), 90),
            header("m3", None, 80),
        ];
        let groups = group_by_thread(msgs);
        assert_eq!(groups.len(), 3);
        for g in &groups {
            assert!(
                matches!(g, MessageListItem::Single(_)),
                "expected all singletons, got {g:?}"
            );
        }
    }

    #[test]
    fn adjacent_same_thread_collapses() {
        let msgs = vec![
            header("m1", Some("t1"), 100),
            header("m2", Some("t1"), 90),
            header("m3", Some("t1"), 80),
            header("m4", Some("t2"), 70),
        ];
        let groups = group_by_thread(msgs);
        assert_eq!(groups.len(), 2);
        match &groups[0] {
            MessageListItem::Thread { head, members } => {
                assert_eq!(head.id.0, "m1", "head should be the newest (first) member");
                assert_eq!(members.len(), 3);
                assert_eq!(members[0].id.0, "m1");
                assert_eq!(members[1].id.0, "m2");
                assert_eq!(members[2].id.0, "m3");
            }
            other => panic!("expected Thread, got {other:?}"),
        }
        assert!(matches!(groups[1], MessageListItem::Single(_)));
    }

    #[test]
    fn non_adjacent_same_thread_does_not_join() {
        let msgs = vec![
            header("m1", Some("t1"), 100),
            header("m2", Some("t2"), 90),
            header("m3", Some("t1"), 80),
        ];
        let groups = group_by_thread(msgs);
        assert_eq!(
            groups.len(),
            3,
            "t1 split by t2 should produce three singletons, not be re-joined"
        );
    }

    #[test]
    fn consecutive_nones_remain_separate() {
        // Two messages with no thread_id are NOT a thread — `None`
        // breaks the run on both sides.
        let msgs = vec![header("m1", None, 100), header("m2", None, 90)];
        let groups = group_by_thread(msgs);
        assert_eq!(groups.len(), 2);
        for g in &groups {
            assert!(matches!(g, MessageListItem::Single(_)));
        }
    }

    #[test]
    fn single_thread_member_still_emits_single() {
        // A run of length 1 against a different next thread must come
        // out as Single, not as a one-member Thread.
        let msgs = vec![
            header("m1", Some("t1"), 100),
            header("m2", Some("t2"), 90),
            header("m3", Some("t2"), 80),
        ];
        let groups = group_by_thread(msgs);
        assert_eq!(groups.len(), 2);
        assert!(matches!(groups[0], MessageListItem::Single(_)));
        assert!(matches!(groups[1], MessageListItem::Thread { .. }));
    }
}
