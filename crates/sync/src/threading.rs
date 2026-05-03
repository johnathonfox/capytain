// SPDX-License-Identifier: Apache-2.0

//! Thread assembly pipeline.
//!
//! `attach_to_thread` runs after each newly-inserted message and
//! decides which `threads` row owns it. The resolution chain
//! follows `PHASE_1.md` Week 13:
//!
//! 1. **`In-Reply-To`** → look up the referenced Message-ID in this
//!    account's local cache. If found, inherit its thread.
//! 2. **`References` (reverse)** → walk the chain newest-first; the
//!    first id that resolves to a known message wins.
//! 3. **Subject + recency** → normalize the subject (strip `Re:` /
//!    `Fwd:`, ASCII lowercase, collapse whitespace) and look for a
//!    thread within the same account whose normalized subject
//!    matches and whose `last_date` is within the **30-day** window.
//! 4. **New thread** → mint a fresh thread row with this message as
//!    the root and the message's normalized subject as the index
//!    key.
//!
//! Once the resolver picks a thread, [`attach_message`] bumps the
//! thread's `message_count` + `last_date` and points the message's
//! `thread_id` at the row.
//!
//! The 30-day window for the subject fallback is deliberately
//! conservative — it lets a flag-flipped reply within an active
//! conversation re-attach without false-positive merging old
//! threads with the same subject ("Re: lunch?" appears every
//! Wednesday).

use std::collections::HashMap;

use chrono::{Duration, Utc};
use tracing::{debug, instrument};

use qsl_core::{MessageHeaders, StorageError, ThreadId};
use qsl_storage::{
    repos::{messages as messages_repo, threads as threads_repo},
    DbConn,
};

/// How far back the subject-fallback step looks. 30 days per spec.
const SUBJECT_RECENCY_DAYS: i64 = 30;

/// Resolve and attach `headers` to a thread. Idempotent — calling
/// twice with the same headers writes the same row twice (the
/// second call's `attach_message` bumps the counter again, which is
/// the cost of not tracking per-message attach state).
///
/// Caller is expected to have just `messages_repo::insert`'d the
/// row — we look up via `find_by_rfc822_id` to honor any
/// uniqueness collisions the insert handled.
#[instrument(skip_all, fields(message = %headers.id.0))]
pub async fn attach_to_thread(
    conn: &dyn DbConn,
    headers: &MessageHeaders,
) -> Result<(), StorageError> {
    if let Some(thread_id) = resolve_existing_thread(conn, headers).await? {
        debug!(
            thread = %thread_id.0,
            "thread match — attaching"
        );
        threads_repo::attach_message(conn, &thread_id, &headers.id, headers.date).await?;
        return Ok(());
    }

    // Fall through: mint a fresh thread.
    let new_id = threads_repo::new_id();
    let thread = threads_repo::Thread {
        id: new_id.clone(),
        account_id: headers.account_id.clone(),
        root_message_id: Some(headers.id.clone()),
        subject_normalized: normalize_subject(&headers.subject),
        last_date: headers.date,
        message_count: 0,
    };
    threads_repo::insert(conn, &thread).await?;
    debug!(thread = %new_id.0, "minted fresh thread");
    threads_repo::attach_message(conn, &new_id, &headers.id, headers.date).await?;
    Ok(())
}

/// Resolution decision for one header on the batched-tx path.
///
/// `sync_folder` runs the resolver in a read-only pre-pass on the
/// shared `&dyn DbConn`, then bakes the chosen `thread_id` into a
/// single transaction's INSERTs / UPDATEs. This enum carries enough
/// info for the tx pass to know whether a fresh `threads` row also
/// needs minting. See [`resolve_with_chunk_local`].
#[derive(Debug, Clone)]
pub struct ThreadAttachment {
    pub thread_id: ThreadId,
    /// `Some(thread_record_to_insert)` if the resolver minted a fresh
    /// thread for this header. The caller's tx pass must INSERT this
    /// row before any message INSERT that references it.
    pub mint: Option<threads_repo::Thread>,
}

/// Read-only resolve with chunk-local visibility.
///
/// Mirrors [`resolve_existing_thread`] but consults `chunk_local`
/// first so a header in the same fetch chunk that references a
/// just-resolved peer (in-chunk back-reference) finds the correct
/// thread without needing the prior message to be committed first.
/// Without this, a tx-batched chunk would lose the within-chunk
/// chaining that the original per-row commit pattern got "for free"
/// by virtue of each insert flushing.
///
/// `mint_for` is invoked when the resolution chain misses; it
/// produces the fresh `Thread` record to insert in the tx pass.
/// Caller threads in the headers it has so the resolver can read
/// them; caller is responsible for keeping `chunk_local` updated as
/// it walks the chunk.
pub async fn resolve_with_chunk_local(
    conn: &dyn DbConn,
    headers: &MessageHeaders,
    chunk_local: &HashMap<String, ThreadId>,
) -> Result<ThreadAttachment, StorageError> {
    // Step 1 (chunk-local then storage): In-Reply-To.
    if let Some(in_reply_to) = headers.in_reply_to.as_deref() {
        if let Some(t) = chunk_local.get(in_reply_to).cloned() {
            return Ok(ThreadAttachment {
                thread_id: t,
                mint: None,
            });
        }
        if let Some(t) = thread_of_message(conn, headers, in_reply_to).await? {
            return Ok(ThreadAttachment {
                thread_id: t,
                mint: None,
            });
        }
    }
    // Step 2: References, walked newest-first.
    for r in headers.references.iter().rev() {
        if let Some(t) = chunk_local.get(r).cloned() {
            return Ok(ThreadAttachment {
                thread_id: t,
                mint: None,
            });
        }
        if let Some(t) = thread_of_message(conn, headers, r).await? {
            return Ok(ThreadAttachment {
                thread_id: t,
                mint: None,
            });
        }
    }
    // Step 3: Subject + recency.
    let normalized = normalize_subject(&headers.subject);
    if !normalized.is_empty() {
        let since = Utc::now() - Duration::days(SUBJECT_RECENCY_DAYS);
        if let Some(thread) =
            threads_repo::find_recent_by_subject(conn, &headers.account_id, &normalized, since)
                .await?
        {
            return Ok(ThreadAttachment {
                thread_id: thread.id,
                mint: None,
            });
        }
    }
    // Step 4: mint a fresh thread.
    let new_id = threads_repo::new_id();
    let mint = threads_repo::Thread {
        id: new_id.clone(),
        account_id: headers.account_id.clone(),
        root_message_id: Some(headers.id.clone()),
        subject_normalized: normalize_subject(&headers.subject),
        last_date: headers.date,
        message_count: 0,
    };
    Ok(ThreadAttachment {
        thread_id: new_id,
        mint: Some(mint),
    })
}

/// Run the resolution chain for an existing thread. Returns
/// `None` only if every step misses, which means the caller mints
/// a new thread.
async fn resolve_existing_thread(
    conn: &dyn DbConn,
    headers: &MessageHeaders,
) -> Result<Option<qsl_core::ThreadId>, StorageError> {
    // Step 1: In-Reply-To.
    if let Some(in_reply_to) = headers.in_reply_to.as_deref() {
        if let Some(t) = thread_of_message(conn, headers, in_reply_to).await? {
            return Ok(Some(t));
        }
    }
    // Step 2: References, walked newest-first.
    for r in headers.references.iter().rev() {
        if let Some(t) = thread_of_message(conn, headers, r).await? {
            return Ok(Some(t));
        }
    }
    // Step 3: Subject + recency.
    let normalized = normalize_subject(&headers.subject);
    if !normalized.is_empty() {
        let since = Utc::now() - Duration::days(SUBJECT_RECENCY_DAYS);
        if let Some(thread) =
            threads_repo::find_recent_by_subject(conn, &headers.account_id, &normalized, since)
                .await?
        {
            return Ok(Some(thread.id));
        }
    }
    Ok(None)
}

/// Look up the thread that owns the local cached row for
/// `rfc822_message_id`. Returns `None` if the referenced message
/// isn't in the cache (typical for the first message synced from a
/// long-running thread) or if it has no `thread_id` yet.
async fn thread_of_message(
    conn: &dyn DbConn,
    headers: &MessageHeaders,
    rfc822_message_id: &str,
) -> Result<Option<qsl_core::ThreadId>, StorageError> {
    let row =
        messages_repo::find_by_rfc822_id(conn, &headers.account_id, rfc822_message_id).await?;
    Ok(row.and_then(|m| m.thread_id))
}

/// Normalize a subject for the subject+recency match path. Strips
/// every leading `Re:` / `Fwd:` / `Fw:` prefix (case-insensitive,
/// any number of repeats), ASCII-lowercases, and collapses runs of
/// whitespace into a single space.
///
/// `PHASE_1.md`'s Open Questions section recorded a deliberate
/// choice between Unicode case fold and plain ASCII lowercase, with
/// the lean toward ASCII for v1 because per-insert performance
/// matters and the localization cost shows up rarely. CJK threads
/// might miss-match on subject; the References-chain step handles
/// well-behaved clients regardless.
pub fn normalize_subject(raw: &str) -> String {
    let mut s = raw.trim();
    loop {
        let stripped = strip_one_prefix(s);
        if stripped.len() == s.len() {
            break;
        }
        s = stripped.trim_start();
    }
    s.to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Strip exactly one `Re:` / `Fwd:` / `Fw:` prefix if present.
/// Case-insensitive; tolerates a colon-and-space or a colon alone.
fn strip_one_prefix(s: &str) -> &str {
    for prefix in ["re:", "fw:", "fwd:"] {
        let plen = prefix.len();
        // `s.get(..plen)` returns `None` either when `plen` falls
        // past the end OR when it lands inside a multibyte char
        // (e.g. subject starts with `’`). Both cases mean "no
        // prefix matched" — falling through to the next iteration
        // is correct; a length-only guard followed by `s[..plen]`
        // would panic on the second case.
        if let Some(head) = s.get(..plen) {
            if head.eq_ignore_ascii_case(prefix) {
                return &s[plen..];
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_subject_strips_re_prefixes() {
        assert_eq!(normalize_subject("Re: Lunch?"), "lunch?");
        assert_eq!(normalize_subject("RE: lunch?"), "lunch?");
        assert_eq!(normalize_subject("Re: Re: Re: Lunch?"), "lunch?");
        assert_eq!(normalize_subject("FWD: lunch?"), "lunch?");
        assert_eq!(normalize_subject("Fw: lunch?"), "lunch?");
        assert_eq!(normalize_subject("Re:Fwd:Lunch?"), "lunch?");
    }

    #[test]
    fn normalize_subject_collapses_whitespace() {
        assert_eq!(normalize_subject("  hello   world  "), "hello world");
        assert_eq!(normalize_subject("hello\tworld"), "hello world");
    }

    #[test]
    fn normalize_subject_handles_leading_multibyte_char() {
        // Real-world Robinhood subject crashed `s[..3]` because byte
        // index 3 lands inside `’` (U+2019, 3 bytes). Regression for
        // the panic surfaced post-OAuth on a fresh sync.
        assert_eq!(
            normalize_subject("We’ve updated our Robinhood Crypto Customer Agreement"),
            "we’ve updated our robinhood crypto customer agreement",
        );
        // Non-ASCII char that happens to coincide with a `re:` prefix
        // length — confirm we don't false-strip.
        assert_eq!(normalize_subject("✅ done"), "✅ done");
    }

    #[test]
    fn normalize_subject_empty_when_blank() {
        assert_eq!(normalize_subject(""), "");
        assert_eq!(normalize_subject("   "), "");
        assert_eq!(normalize_subject("Re: "), "");
    }
}
