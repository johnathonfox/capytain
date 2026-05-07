// SPDX-License-Identifier: Apache-2.0

//! `EXPLAIN QUERY PLAN` checks for the slow-query suspects.
//!
//! Confirms which index Turso 0.5.3's planner actually picks for the
//! queries flagged in /tmp/qsl.log. Apply every shipped migration to
//! a fresh in-memory DB, load enough synthetic rows that the planner
//! has stats to chew on, then run `EXPLAIN QUERY PLAN` on each
//! suspect query and print the plan.
//!
//! Outputs go to stdout via `cargo test -- --nocapture`. The test
//! always passes — the goal is the plan output, not a pass/fail.

use qsl_core::{AccountId, FolderId};
use qsl_storage::{run_migrations, DbConn, Params, TursoConn, Value};

// Plan choice in Turso 0.5.3 is heuristic (no ANALYZE stats), so a
// small dataset is fine for telling us which index the planner picks.
const N_ACCOUNTS: usize = 1;
const N_FOLDERS: usize = 4;
const N_MESSAGES: usize = 200;

async fn explain(conn: &TursoConn, label: &str, sql: &str, params: Params<'_>) {
    let explain_sql = format!("EXPLAIN QUERY PLAN {sql}");
    let rows = conn.query(&explain_sql, params).await.expect("explain");
    println!("--- {label} ---");
    println!("SQL: {sql}");
    for r in rows {
        // EXPLAIN QUERY PLAN columns in SQLite/Turso: id, parent, notused, detail
        let detail = r
            .get_optional_str("detail")
            .ok()
            .flatten()
            .unwrap_or_default()
            .to_string();
        let id = r.get_i64("id").unwrap_or(-1);
        let parent = r.get_i64("parent").unwrap_or(-1);
        println!("  [{id} <- {parent}] {detail}");
    }
    println!();
}

// `N_ACCOUNTS = 1` makes the modulo arithmetic in the seeding loop
// constant; silence the lint at the test's outer scope.
#[allow(clippy::modulo_one)]
#[tokio::test]
async fn explain_slow_queries() {
    let conn = TursoConn::in_memory().await.expect("in-memory db");
    run_migrations(&conn).await.expect("migrate");

    // Seed minimal account + folders.
    for ai in 0..N_ACCOUNTS {
        let aid = format!("acct-{ai}");
        conn.execute(
            "INSERT INTO accounts (id, kind, display_name, email_address, created_at)
             VALUES (?1, 'imap_smtp', 'a', 'a@x.test', 0)",
            Params(vec![Value::Text(&aid)]),
        )
        .await
        .expect("insert account");
        for fi in 0..N_FOLDERS {
            let fid = format!("acct-{ai}/folder-{fi}");
            conn.execute(
                "INSERT INTO folders (id, account_id, name, path) VALUES (?1, ?2, ?3, ?3)",
                Params(vec![
                    Value::Text(&fid),
                    Value::Text(&aid),
                    Value::Text(&fid),
                ]),
            )
            .await
            .expect("insert folder");
        }
    }

    // Seed messages: spread across accounts/folders, with a date so
    // ORDER BY date DESC has something real to sort.
    for mi in 0..N_MESSAGES {
        let aid = format!("acct-{}", mi % N_ACCOUNTS);
        let fid = format!("acct-{}/folder-{}", mi % N_ACCOUNTS, mi % N_FOLDERS);
        let msgid = format!("msg-{mi}");
        let rfcid = format!("<rfc-{mi}@x.test>");
        // Use NULL thread_id everywhere — keeps the test schema-compliant
        // without minting threads. Doesn't matter for the index plans we
        // want to read.
        conn.execute(
            "INSERT INTO messages (id, account_id, folder_id, thread_id, rfc822_message_id, \
                                   subject, from_json, reply_to_json, to_json, cc_json, bcc_json, \
                                   date, flags_json, labels_json, snippet, size, has_attachments, \
                                   in_reply_to, references_json, unread)
             VALUES (?1, ?2, ?3, NULL, ?4, 's', '[]', '[]', '[]', '[]', '[]', \
                     ?5, '{}', '[]', 'snip', 0, 0, NULL, '[]', 1)",
            Params(vec![
                Value::Text(&msgid),
                Value::Text(&aid),
                Value::Text(&fid),
                Value::Text(&rfcid),
                Value::Integer(mi as i64),
            ]),
        )
        .await
        .expect("insert message");
    }

    // Dump engine settings so we know what Turso 0.5.3 actually
    // honors. Many of these are PRAGMA-level knobs the project
    // assumes work but never verified; the comments in
    // `crates/storage/src/turso_conn.rs::open` even flag that
    // unrecognized PRAGMAs are silently no-ops.
    println!("\n=== Turso PRAGMA introspection ===\n");
    for pragma in [
        "PRAGMA journal_mode",
        "PRAGMA synchronous",
        "PRAGMA cache_size",
        "PRAGMA temp_store",
        "PRAGMA page_size",
        "PRAGMA mmap_size",
        "PRAGMA threads",
        "PRAGMA foreign_keys",
        "PRAGMA wal_autocheckpoint",
        "PRAGMA busy_timeout",
        "PRAGMA application_id",
    ] {
        match conn.query(pragma, Params(vec![])).await {
            Ok(rows) => {
                if rows.is_empty() {
                    println!("  {pragma:<28} (no rows returned)");
                } else {
                    for r in rows.iter() {
                        // Try common column names; PRAGMAs return either an
                        // unnamed column or a named one matching the pragma.
                        let val = r
                            .get_optional_str("journal_mode")
                            .ok()
                            .flatten()
                            .map(str::to_owned)
                            .or_else(|| {
                                r.get_optional_str("synchronous")
                                    .ok()
                                    .flatten()
                                    .map(str::to_owned)
                            })
                            .or_else(|| r.get_i64("cache_size").ok().map(|n| n.to_string()))
                            .or_else(|| r.get_i64("temp_store").ok().map(|n| n.to_string()))
                            .or_else(|| r.get_i64("page_size").ok().map(|n| n.to_string()))
                            .or_else(|| r.get_i64("mmap_size").ok().map(|n| n.to_string()))
                            .or_else(|| r.get_i64("threads").ok().map(|n| n.to_string()))
                            .or_else(|| r.get_i64("foreign_keys").ok().map(|n| n.to_string()))
                            .or_else(|| r.get_i64("wal_autocheckpoint").ok().map(|n| n.to_string()))
                            .or_else(|| r.get_i64("busy_timeout").ok().map(|n| n.to_string()))
                            .or_else(|| r.get_i64("application_id").ok().map(|n| n.to_string()))
                            .unwrap_or_else(|| "<unknown shape>".to_string());
                        println!("  {pragma:<28} = {val}");
                    }
                }
            }
            Err(e) => {
                println!("  {pragma:<28} ERROR: {e}");
            }
        }
    }
    println!();

    // ---- VERIFY PRAGMA WRITES ----
    // `busy_timeout` introspection above showed Turso silently
    // accepts the PRAGMA syntax but reports 0 at runtime. Test
    // whether the two cache/sort knobs we'd actually want to use
    // (cache_size, temp_store) survive a write.
    println!("\n=== PRAGMA write verification ===\n");
    let writes: &[(&str, &str, &str)] = &[
        // (set_sql, read_sql, expected_substring)
        (
            "PRAGMA cache_size = -128000",
            "PRAGMA cache_size",
            "-128000",
        ),
        (
            "PRAGMA temp_store = MEMORY",
            "PRAGMA temp_store",
            "2", // MEMORY = 2 in SQLite
        ),
        (
            "PRAGMA mmap_size = 2147483648",
            "PRAGMA mmap_size",
            "2147483648",
        ),
    ];
    for (set, read, expected) in writes {
        match conn.execute(set, Params(vec![])).await {
            Ok(_) => {
                let got = match conn.query(read, Params(vec![])).await {
                    Ok(rows) if !rows.is_empty() => {
                        let r = &rows[0];
                        r.get_optional_str("temp_store")
                            .ok()
                            .flatten()
                            .map(str::to_owned)
                            .or_else(|| r.get_i64("cache_size").ok().map(|n| n.to_string()))
                            .or_else(|| r.get_i64("temp_store").ok().map(|n| n.to_string()))
                            .or_else(|| r.get_i64("mmap_size").ok().map(|n| n.to_string()))
                            .unwrap_or_else(|| "<unknown>".to_string())
                    }
                    Ok(_) => "<no rows>".to_string(),
                    Err(e) => format!("read failed: {e}"),
                };
                let took = if got.contains(expected) {
                    "TOOK"
                } else {
                    "DROPPED"
                };
                println!("  {set:<40} → {got} ({took}, expected ~{expected})");
            }
            Err(e) => {
                println!("  {set:<40} → SET FAILED: {e}");
            }
        }
    }
    println!();

    println!("\n=== plans for queries flagged as slow in /tmp/qsl.log ===\n");

    // 1. messages_list — the user-visible folder-switch query.
    let folder = FolderId("acct-0/folder-0".into());
    explain(
        &conn,
        "messages_list (list_by_folder)",
        "SELECT id, account_id, folder_id, thread_id, rfc822_message_id, subject, \
         from_json, reply_to_json, to_json, cc_json, bcc_json, date, flags_json, \
         labels_json, snippet, size, has_attachments, body_path, in_reply_to, \
         references_json FROM messages WHERE folder_id = ?1 ORDER BY date DESC LIMIT ?2 OFFSET ?3",
        Params(vec![
            Value::Text(&folder.0),
            Value::Integer(50),
            Value::Integer(0),
        ]),
    )
    .await;

    // 2. reconciliation prune.
    explain(
        &conn,
        "reconciliation prune (list_ids_by_folder)",
        "SELECT id FROM messages WHERE folder_id = ?1",
        Params(vec![Value::Text(&folder.0)]),
    )
    .await;

    // 3. apply_chunk batch_find_existing — the post-#147 wider SELECT.
    explain(
        &conn,
        "batch_find_existing (post-#147 wider SELECT)",
        "SELECT id, thread_id, folder_id, flags_json, labels_json FROM messages \
         WHERE id IN (?1, ?2, ?3)",
        Params(vec![
            Value::Text("msg-1"),
            Value::Text("msg-2"),
            Value::Text("msg-3"),
        ]),
    )
    .await;

    // 4. threading: NEW narrow find_thread_id_by_rfc822_id (post-#154).
    let acct = AccountId("acct-0".into());
    explain(
        &conn,
        "threading::thread_of_message (NARROW find_thread_id_by_rfc822_id)",
        "SELECT thread_id FROM messages \
         WHERE account_id = ?1 AND rfc822_message_id = ?2 LIMIT 1",
        Params(vec![Value::Text(&acct.0), Value::Text("<rfc-1@x.test>")]),
    )
    .await;

    // 4b. Old wide find_by_rfc822_id — kept around for backward compat,
    // confirm its plan still has SORTER.
    explain(
        &conn,
        "threading: OLD wide find_by_rfc822_id (kept for compat)",
        "SELECT id, account_id, folder_id, thread_id, rfc822_message_id, subject, \
         from_json, reply_to_json, to_json, cc_json, bcc_json, date, flags_json, \
         labels_json, snippet, size, has_attachments, body_path, in_reply_to, \
         references_json FROM messages WHERE account_id = ?1 AND rfc822_message_id = ?2 \
         ORDER BY date DESC LIMIT 1",
        Params(vec![Value::Text(&acct.0), Value::Text("<rfc-1@x.test>")]),
    )
    .await;

    // 5. count_unread_by_folder — should hit messages_folder_unread.
    explain(
        &conn,
        "count_unread_by_folder",
        "SELECT COUNT(*) AS c FROM messages WHERE folder_id = ?1 AND unread = 1",
        Params(vec![Value::Text(&folder.0)]),
    )
    .await;

    // 6. count_by_folder.
    explain(
        &conn,
        "count_by_folder",
        "SELECT COUNT(*) AS c FROM messages WHERE folder_id = ?1",
        Params(vec![Value::Text(&folder.0)]),
    )
    .await;

    // 7. Slim id+date variant — does Turso still add a SORTER when the
    // SELECT list matches the index columns? If not, a two-stage query
    // (id+date here, hydrate by PK) avoids the wide-row sort.
    explain(
        &conn,
        "slim list (id, date) with same WHERE/ORDER",
        "SELECT id, date FROM messages WHERE folder_id = ?1 \
         ORDER BY date DESC LIMIT ?2 OFFSET ?3",
        Params(vec![
            Value::Text(&folder.0),
            Value::Integer(50),
            Value::Integer(0),
        ]),
    )
    .await;

    // 8. PK hydration after step 7 — what plan does Turso produce for the
    // wide SELECT keyed by PK list?
    explain(
        &conn,
        "hydrate by PK list (post-slim)",
        "SELECT id, account_id, folder_id, thread_id, rfc822_message_id, subject, \
         from_json, reply_to_json, to_json, cc_json, bcc_json, date, flags_json, \
         labels_json, snippet, size, has_attachments, body_path, in_reply_to, \
         references_json FROM messages WHERE id IN (?1, ?2, ?3)",
        Params(vec![
            Value::Text("msg-1"),
            Value::Text("msg-2"),
            Value::Text("msg-3"),
        ]),
    )
    .await;

    // ---- ANALYZE experiment ----
    println!("\n=== running ANALYZE on messages ===");
    match conn.execute("ANALYZE messages", Params(vec![])).await {
        Ok(_) => println!("ANALYZE messages: OK\n"),
        Err(e) => {
            println!("ANALYZE messages: FAILED ({e})\n");
            return;
        }
    }

    println!("=== plans for the same queries POST-ANALYZE ===\n");

    // Re-run the two suspects most likely to benefit from stats:
    // messages_list (wide ORDER BY DESC) and the slim id+date.
    let folder = FolderId("acct-0/folder-0".into());
    explain(
        &conn,
        "POST-ANALYZE messages_list",
        "SELECT id, account_id, folder_id, thread_id, rfc822_message_id, subject, \
         from_json, reply_to_json, to_json, cc_json, bcc_json, date, flags_json, \
         labels_json, snippet, size, has_attachments, body_path, in_reply_to, \
         references_json FROM messages WHERE folder_id = ?1 ORDER BY date DESC LIMIT ?2 OFFSET ?3",
        Params(vec![
            Value::Text(&folder.0),
            Value::Integer(50),
            Value::Integer(0),
        ]),
    )
    .await;

    explain(
        &conn,
        "POST-ANALYZE slim list (id, date)",
        "SELECT id, date FROM messages WHERE folder_id = ?1 \
         ORDER BY date DESC LIMIT ?2 OFFSET ?3",
        Params(vec![
            Value::Text(&folder.0),
            Value::Integer(50),
            Value::Integer(0),
        ]),
    )
    .await;
}
