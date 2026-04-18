// SPDX-License-Identifier: Apache-2.0

//! `mailcli` — Capytain's headless protocol CLI.
//!
//! Phase 0 scope (all of Week 4 landed end-to-end here):
//!
//! - `auth add <provider> <email>` runs the OAuth2 + PKCE flow against
//!   the built-in provider profile, stores the refresh token in the
//!   keychain, and writes an account row to the local database.
//! - `auth list` prints accounts on disk with a keychain presence flag.
//! - `auth remove <email>` deletes both the DB row and the keychain
//!   entry.
//! - `list-folders <email>` connects via the appropriate adapter (IMAP
//!   for Gmail, JMAP for Fastmail) using an access token refreshed
//!   against the stored refresh token, and prints every folder the
//!   server advertises.
//! - `list-messages <email> <folder> [--limit N]` SELECT+FETCHes
//!   (IMAP) or Email/query+get-s (JMAP) the most recent N messages.
//! - `sync <email>` finds the INBOX, upserts its folder row, runs
//!   delta sync against any previously-persisted cursor, writes each
//!   message header via `capytain_storage::repos::messages`, and
//!   persists the new sync cursor. Prints
//!   `Synced N new messages, M removed, in T ms`.
//! - `show-message <id>` is a placeholder — IMAP ids encode the
//!   folder so it would be self-contained, but we want a consistent
//!   UX across both adapters; the full version lands with the Phase
//!   1 polish pass.

use std::path::PathBuf;

use chrono::Utc;
use clap::{Parser, Subcommand};
use directories::ProjectDirs;
use tracing::info;

use capytain_auth::{
    lookup as provider_lookup, refresh_access_token, run_loopback_flow, AuthError, TokenVault,
};
use capytain_core::{Account, AccountId, BackendKind, FolderId, MailBackend, MailError};
use capytain_imap_client::ImapBackend;
use capytain_jmap_client::JmapBackend;
use capytain_storage::{repos, run_migrations, TursoConn};

/// Capytain headless protocol CLI.
#[derive(Debug, Parser)]
#[command(name = "mailcli", version, about, long_about = None)]
struct Cli {
    /// Tracing filter directive, e.g. `info`, `debug`, or
    /// `capytain_imap_client=trace,warn`. Takes precedence over
    /// `RUST_LOG`.
    #[arg(long, value_name = "FILTER", global = true)]
    log_level: Option<String>,

    /// Override the Capytain data directory. Defaults to the
    /// OS-idiomatic location (XDG on Linux, Application Support on
    /// macOS, AppData on Windows).
    #[arg(long, value_name = "DIR", global = true)]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// OAuth2 account management.
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },

    /// List folders (mailboxes) on the server. Phase 0 stub.
    ListFolders {
        /// Email address of the previously-added account.
        email: String,
    },

    /// List messages in a folder. Phase 0 stub.
    ListMessages {
        email: String,
        folder: String,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },

    /// Show a single message by ID. Phase 0 stub.
    ShowMessage { id: String },

    /// Sync an account. Phase 0 stub.
    Sync { email: String },
}

#[derive(Debug, Subcommand)]
enum AuthAction {
    /// Run the OAuth2 + PKCE flow for a provider and store the refresh
    /// token in the OS keychain.
    Add {
        /// One of `gmail`, `fastmail`.
        provider: String,
        /// Email address the user is authenticating.
        email: String,
    },

    /// List locally-known accounts and whether the keychain has a
    /// refresh token for each.
    List,

    /// Remove an account locally: delete the row from storage and the
    /// refresh token from the keychain.
    Remove {
        /// Email address of the account to remove.
        email: String,
    },
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    if let Err(e) = capytain_telemetry::init(cli.log_level.as_deref()) {
        eprintln!("mailcli: failed to initialize tracing: {e}");
        return std::process::ExitCode::from(1);
    }

    match run(cli).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mailcli: {e}");
            std::process::ExitCode::from(1)
        }
    }
}

async fn run(cli: Cli) -> Result<(), MailcliError> {
    let paths = DataPaths::resolve(cli.data_dir.as_deref())?;
    match cli.command {
        Command::Auth { action } => dispatch_auth(action, &paths).await,
        Command::ListFolders { email } => list_folders(&email, &paths).await,
        Command::ListMessages {
            email,
            folder,
            limit,
        } => list_messages(&email, &folder, limit, &paths).await,
        Command::ShowMessage { id } => show_message(&id, &paths).await,
        Command::Sync { email } => sync_account(&email, &paths).await,
    }
}

async fn dispatch_auth(action: AuthAction, paths: &DataPaths) -> Result<(), MailcliError> {
    match action {
        AuthAction::Add { provider, email } => auth_add(&provider, &email, paths).await,
        AuthAction::List => auth_list(paths).await,
        AuthAction::Remove { email } => auth_remove(&email, paths).await,
    }
}

async fn auth_add(provider_slug: &str, email: &str, paths: &DataPaths) -> Result<(), MailcliError> {
    let provider = capytain_auth::lookup(provider_slug)
        .ok_or_else(|| MailcliError::Usage(format!("unknown provider: {provider_slug}")))?;

    info!(provider = provider_slug, %email, "starting OAuth2 + PKCE flow");
    let outcome = run_loopback_flow(provider, Some(email)).await?;

    let refresh = outcome.tokens.refresh.ok_or_else(|| {
        MailcliError::Auth(AuthError::TokenExchange(
            "provider did not return a refresh_token; \
             re-run with `prompt=consent` on the server side or add the `offline_access` scope"
                .into(),
        ))
    })?;

    let account_id = AccountId(format!("{provider_slug}:{email}"));
    // `ProviderKind` is `#[non_exhaustive]`. The fallback keeps future
    // variants (e.g. EAS when/if it becomes relevant — unlikely per
    // DESIGN.md §2, but cheap insurance) from breaking this crate.
    let kind = match provider.profile().kind {
        capytain_auth::ProviderKind::ImapSmtp => BackendKind::ImapSmtp,
        capytain_auth::ProviderKind::Jmap => BackendKind::Jmap,
        _ => {
            return Err(MailcliError::Usage(format!(
                "provider {provider_slug} uses an unsupported backend kind"
            )))
        }
    };
    let account = Account {
        id: account_id.clone(),
        kind: kind.clone(),
        display_name: email.to_string(),
        email_address: email.to_string(),
        created_at: Utc::now(),
    };

    let conn = paths.open_db().await?;
    let vault = TokenVault::new();
    match repos::accounts::find(&conn, &account.id).await? {
        Some(_) => {
            info!("account already present; updating");
            repos::accounts::update(&conn, &account).await?;
        }
        None => {
            repos::accounts::insert(&conn, &account).await?;
        }
    }
    vault.put(&account_id, &refresh)?;

    println!(
        "added {email} ({} provider, backend={kind:?}, {} scope(s))",
        provider.profile().name,
        outcome.granted_scopes.len(),
    );
    Ok(())
}

async fn auth_list(paths: &DataPaths) -> Result<(), MailcliError> {
    let conn = paths.open_db().await?;
    let vault = TokenVault::new();

    let accounts = repos::accounts::list(&conn).await?;
    if accounts.is_empty() {
        println!("no accounts configured. Use `mailcli auth add <provider> <email>`.");
        return Ok(());
    }
    println!(
        "{:<40}  {:<10}  {:<8}  email",
        "account_id", "backend", "keychain"
    );
    for a in accounts {
        let has_token = vault.contains(&a.id).unwrap_or(false);
        println!(
            "{:<40}  {:<10}  {:<8}  {}",
            a.id.0,
            format!("{:?}", a.kind),
            if has_token { "ok" } else { "MISSING" },
            a.email_address
        );
    }
    Ok(())
}

async fn auth_remove(email: &str, paths: &DataPaths) -> Result<(), MailcliError> {
    let conn = paths.open_db().await?;
    let vault = TokenVault::new();

    // Accounts are minted as `<provider>:<email>` but the user only
    // types the email — look up by email and remove every match.
    let accounts = repos::accounts::list(&conn).await?;
    let matches: Vec<_> = accounts
        .into_iter()
        .filter(|a| a.email_address == email)
        .collect();
    if matches.is_empty() {
        return Err(MailcliError::Usage(format!("no account found for {email}")));
    }
    for a in matches {
        vault.delete(&a.id)?;
        repos::accounts::delete(&conn, &a.id).await?;
        println!("removed {}", a.id.0);
    }
    Ok(())
}

// ---------- read path commands ----------

async fn resolve_account(
    conn: &TursoConn,
    email: &str,
) -> Result<capytain_core::Account, MailcliError> {
    let accounts = repos::accounts::list(conn).await?;
    accounts
        .into_iter()
        .find(|a| a.email_address == email)
        .ok_or_else(|| {
            MailcliError::Usage(format!(
                "no account found for {email}; run `mailcli auth add`"
            ))
        })
}

fn provider_slug_from_id(id: &AccountId) -> Option<&str> {
    id.0.split_once(':').map(|(slug, _)| slug)
}

/// Build a live [`MailBackend`] for an account by refreshing its access
/// token and handing it to the right adapter.
async fn open_backend(account: &Account) -> Result<Box<dyn MailBackend>, MailcliError> {
    let slug = provider_slug_from_id(&account.id).ok_or_else(|| {
        MailcliError::Usage(format!(
            "account id {} does not follow `<provider>:<email>`",
            account.id.0
        ))
    })?;
    let provider = provider_lookup(slug)
        .ok_or_else(|| MailcliError::Usage(format!("unknown provider: {slug}")))?;
    let vault = TokenVault::new();
    let token_set = refresh_access_token(provider, &vault, &account.id).await?;

    match account.kind {
        BackendKind::ImapSmtp => {
            let host = match slug {
                "gmail" => "imap.gmail.com",
                other => {
                    return Err(MailcliError::Usage(format!(
                        "no hardcoded IMAP host for provider {other}; \
                         set one in capytain-imap-client before connecting"
                    )))
                }
            };
            let backend = ImapBackend::connect_tls(
                host,
                993,
                &account.email_address,
                token_set.access.expose(),
                account.id.clone(),
            )
            .await?;
            Ok(Box::new(backend))
        }
        BackendKind::Jmap => {
            let session_url = match slug {
                "fastmail" => "https://api.fastmail.com/.well-known/jmap",
                other => {
                    return Err(MailcliError::Usage(format!(
                        "no hardcoded JMAP session URL for provider {other}"
                    )))
                }
            };
            let backend =
                JmapBackend::connect(session_url, token_set.access.expose(), account.id.clone())
                    .await
                    .map_err(MailcliError::Mail)?;
            Ok(Box::new(backend))
        }
        _ => Err(MailcliError::Usage(format!(
            "account {} uses an unsupported backend kind",
            account.id.0
        ))),
    }
}

async fn list_folders(email: &str, paths: &DataPaths) -> Result<(), MailcliError> {
    let conn = paths.open_db().await?;
    let account = resolve_account(&conn, email).await?;
    let backend = open_backend(&account).await?;
    let folders = backend.list_folders().await?;

    println!(
        "{:<24}  {:<12}  {:>7}  {:>7}  path",
        "id", "role", "unread", "total"
    );
    for f in folders {
        let role = f
            .role
            .as_ref()
            .map(|r| format!("{r:?}"))
            .unwrap_or_default();
        let truncated_id: String = f.id.0.chars().take(24).collect();
        println!(
            "{:<24}  {:<12}  {:>7}  {:>7}  {}",
            truncated_id, role, f.unread_count, f.total_count, f.path
        );
    }
    Ok(())
}

async fn list_messages(
    email: &str,
    folder: &str,
    limit: u32,
    paths: &DataPaths,
) -> Result<(), MailcliError> {
    let conn = paths.open_db().await?;
    let account = resolve_account(&conn, email).await?;
    let backend = open_backend(&account).await?;
    let fid = FolderId(folder.to_string());

    let out = backend.list_messages(&fid, None, Some(limit)).await?;
    println!("{:<12}  {:<20}  {:<32}  subject", "flags", "from", "date");
    for m in &out.messages {
        let flags = format!(
            "{}{}{}",
            if m.flags.seen { "R" } else { "U" },
            if m.flags.flagged { "*" } else { " " },
            if m.flags.answered { "↩" } else { " " },
        );
        let from = m
            .from
            .first()
            .map(|a| a.address.clone())
            .unwrap_or_default();
        let from: String = from.chars().take(20).collect();
        println!(
            "{:<12}  {:<20}  {:<32}  {}",
            flags,
            from,
            m.date.to_rfc3339(),
            m.subject
        );
    }
    println!(
        "({} messages; new sync cursor persisted on next `sync`)",
        out.messages.len()
    );
    Ok(())
}

async fn show_message(id: &str, _paths: &DataPaths) -> Result<(), MailcliError> {
    // Decoding the id tells us which account (by provider slug for
    // IMAP; opaque for JMAP). For Phase 0 we just say "look it up once
    // you know the account" — the user passes an account email
    // separately in future revisions. Today the command is a
    // placeholder to prove the CLI shape.
    Err(MailcliError::Usage(format!(
        "`show-message {id}` needs an --email selector; \
         today `list-messages` prints ids in a debuggable format and \
         `show-message` with an account hint lands in Phase 1 Week 1"
    )))
}

async fn sync_account(email: &str, paths: &DataPaths) -> Result<(), MailcliError> {
    use std::time::Instant;

    let conn = paths.open_db().await?;
    let account = resolve_account(&conn, email).await?;
    let backend = open_backend(&account).await?;

    // Phase 0 exit criterion: fetch the INBOX's headers and persist
    // them. We run delta against the stored sync_state if present.
    let folders = backend.list_folders().await?;
    let inbox = folders
        .into_iter()
        .find(|f| {
            f.role == Some(capytain_core::FolderRole::Inbox) || f.path.eq_ignore_ascii_case("INBOX")
        })
        .ok_or_else(|| {
            MailcliError::Usage("could not find an INBOX folder on this account".into())
        })?;

    // Persist the folder row so sync_states has a place to hang the
    // backend cursor on.
    match repos::folders::find(&conn, &inbox.id).await? {
        Some(_) => repos::folders::update(&conn, &inbox).await?,
        None => repos::folders::insert(&conn, &inbox).await?,
    }

    let prior = repos::sync_states::get(&conn, &inbox.id)
        .await
        .ok()
        .flatten();

    let start = Instant::now();
    let result = backend
        .list_messages(&inbox.id, prior.as_ref(), Some(200))
        .await?;
    let added = result.messages.len();
    let removed = result.removed.len();

    // Persist headers.
    for h in &result.messages {
        // Upsert — storage's repos don't have one, so check then
        // insert/update.
        match repos::messages::find(&conn, &h.id).await? {
            Some(_) => repos::messages::update(&conn, h, None).await?,
            None => repos::messages::insert(&conn, h, None).await?,
        }
    }
    repos::sync_states::put(&conn, &result.new_state).await?;

    let duration = start.elapsed();
    println!(
        "Synced {added} new messages, {removed} removed, in {} ms",
        duration.as_millis()
    );
    Ok(())
}

// ---------- paths + DB ----------

struct DataPaths {
    data_dir: PathBuf,
}

impl DataPaths {
    fn resolve(override_dir: Option<&std::path::Path>) -> Result<Self, MailcliError> {
        let data_dir = match override_dir {
            Some(p) => p.to_path_buf(),
            None => ProjectDirs::from("app", "capytain", "capytain")
                .ok_or_else(|| {
                    MailcliError::Usage(
                        "could not resolve OS data directory; pass --data-dir".into(),
                    )
                })?
                .data_dir()
                .to_path_buf(),
        };
        std::fs::create_dir_all(&data_dir)
            .map_err(|e| MailcliError::Usage(format!("create {data_dir:?}: {e}")))?;
        Ok(Self { data_dir })
    }

    fn db_path(&self) -> PathBuf {
        self.data_dir.join("capytain.db")
    }

    async fn open_db(&self) -> Result<TursoConn, MailcliError> {
        let path = self.db_path();
        let conn = TursoConn::open(&path).await?;
        run_migrations(&conn).await?;
        Ok(conn)
    }
}

// ---------- error type ----------

#[derive(Debug, thiserror::Error)]
enum MailcliError {
    #[error("{0}")]
    Usage(String),
    #[error(transparent)]
    Mail(#[from] MailError),
    #[error(transparent)]
    Storage(#[from] capytain_core::StorageError),
    #[error(transparent)]
    Auth(#[from] AuthError),
}
