// SPDX-License-Identifier: Apache-2.0

//! Tauri command handlers.
//!
//! One module per domain area of `COMMANDS.md`. Each command is a thin
//! adapter: parse input, delegate to `capytain-core` / `capytain-storage`
//! / `capytain-*-client`, map errors through `IpcError`, return
//! `IpcResult<T>`.
//!
//! # Phase 0 Week 5 part 1 scope
//!
//! Only `accounts_list` lands here. The rest of the command catalogue —
//! `accounts_add_oauth`, `folders_list`, `messages_*`, etc. — arrives in
//! Week 5 part 2 once the sidebar and message list components need
//! them.

pub mod accounts;
pub mod folders;
pub mod messages;
pub mod reader;
