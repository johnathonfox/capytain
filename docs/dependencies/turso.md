<!--
SPDX-FileCopyrightText: 2026 QSL Contributors
SPDX-License-Identifier: Apache-2.0
-->

# Turso Engagement Log

QSL's storage layer runs on [Turso](https://github.com/tursodatabase/turso), a pure-Rust SQLite-compatible embedded database. Turso is in active development and shipping sub-1.0 releases; this document tracks how we engage with it, what's currently broken, and when we vendor a patch.

## Pinned version

- **Crate:** `turso` 0.5.x (currently 0.5.3)
- **Workspace declaration:** `Cargo.toml` `[workspace.dependencies]` → `turso = "0.5"` (tilde-compatible within 0.5.x)

Bumping the minor or major version requires re-running the full storage test suite (`cargo test -p qsl-storage`) and updating this file.

## Upstream-patch-after-N-days policy

Per [`DESIGN.md` §12](../../DESIGN.md#12-open-questions):

> **N = 14 days.** If a Turso bug or missing feature blocks us and upstream has not landed (or clearly committed to landing) a fix within 14 days of a tracked issue, we vendor a patch. The vendored patch ships as a `[patch.crates-io]` entry plus a fork on `github.com/johnathonfox/turso` with the QSL fix cherry-picked on top. We upstream the patch in parallel and drop our fork as soon as the official release lands.

We prefer upstreaming over long-lived forks. A vendored patch is an admission of a maintenance cost, not a strategic choice.

## Known issues as of 2026-04-18

### Transactions are placeholder-only

Turso 0.5.3 ships `pub struct Transaction {}` with no methods — the transaction API is declared but unimplemented. `Connection::unchecked_transaction()` and `Connection::transaction()` both return this empty struct.

**Workaround:** `qsl-storage` drives transactions at the SQL level via raw `BEGIN` / `COMMIT` / `ROLLBACK` statements on the underlying connection. This matches SQLite's actual transaction model and is correct, just less ergonomic than a typed Transaction wrapper would be.

**Impact:** `crates/storage/src/turso_conn.rs` has a `TursoTx` struct that holds a borrowed `&turso::Connection` and issues text commands. If Turso ships a real Transaction API later, we'll swap the implementation under the same `DbConn`/`Tx` trait surface without any callers changing.

**Upstream tracking:** (issue link TBD once we open it)

### Multi-statement `execute()` truncates inside a transaction

Calling `Connection::execute(sql, ())` with multi-statement SQL (`CREATE …; CREATE …;`) at the top level applies every statement. Inside `BEGIN` / `COMMIT`, only the **first** statement is executed; the rest are silently ignored.

**Workaround:** `crates/storage/src/migrations.rs` splits the migration file on `;` (after stripping `--` line comments) and issues each statement through the transaction individually. Every DDL in the shipped migrations uses `IF NOT EXISTS`, so a retry after a partial failure is safe.

**Impact:** One more failure-mode footgun when adding migrations. The convention doc in `CONTRIBUTING.md` should call out that migration statements must be independently re-runnable.

**Upstream tracking:** (issue link TBD)

### MSRV bumped to 1.88

`turso_macros` 0.5.3 uses `proc_macro::Span::file()`, which stabilized in Rust 1.88. This forced us to bump `rust-toolchain.toml` from 1.87.0 (the pin we chose in PR #1) to 1.88.0. Not a bug per se, just worth noting as a downstream cost.

## Local workflow for investigating a Turso issue

1. Write the minimum-reproducing test in `crates/storage/tests/` (or a throwaway `/tmp/turso_probe/` crate).
2. Run against the pinned 0.5.x in our `Cargo.lock`.
3. If the behavior repros against `main` on `github.com/tursodatabase/turso`, open an upstream issue with the repro.
4. Add an entry to "Known issues" above with the issue link.
5. If 14 days pass without upstream progress, open a fork and wire in a `[patch.crates-io]` entry. File the patch PR against upstream at the same time.

## Change log

- **2026-04-18** — Initial file. Pinned `turso = "0.5"` (resolving to 0.5.3). Documented the transaction placeholder and multi-statement-in-transaction bugs as worked-around locally. N=14.
- **2026-04-27** — Adopted Turso's experimental Tantivy-backed FTS for QSL search (PR-S1). Required (a) opting into the `fts` cargo feature on a sibling `turso_core` workspace dep, and (b) calling `Builder::experimental_index_method(true)` on every connection open in `crates/storage/src/turso_conn.rs`. Migration `0005_search_fts.sql` indexes `subject` / `from_json` / `to_json` / `snippet` on the `messages` table; the engine auto-tracks inserts / updates / deletes, so no write-side hooks were needed. Note: SQLite's `WHERE table MATCH 'q'` is unsupported — query through the `fts_match()` / `fts_score()` table functions instead.
