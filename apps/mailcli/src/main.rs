// SPDX-License-Identifier: Apache-2.0

//! `mailcli` — Capytain's headless protocol CLI.
//!
//! Phase 0 scope:
//!
//! - `auth add <provider> <email>` runs the OAuth2 + PKCE flow against
//!   the built-in provider profile, stores the refresh token in the
//!   keychain, and writes an account row to the local database.
//! - `auth list` prints the accounts on disk with a keychain presence
//!   indicator.
//! - `auth remove <email>` deletes both the account row and the keychain
//!   entry.
//!
//! Week 4+ will flesh out `list-folders`, `list-messages`, `show-message`,
//! and `sync`. They're scaffolded here as subcommands that print a
//! Phase-0 status line so the shape of the CLI is already visible.

use std::path::PathBuf;

use chrono::Utc;
use clap::{Parser, Subcommand};
use directories::ProjectDirs;
use tracing::info;

use capytain_auth::{run_loopback_flow, AuthError, TokenVault};
use capytain_core::{Account, AccountId, BackendKind, MailError};
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
        Command::ListFolders { email } => stub("list-folders", &email, "Phase 0 Week 4"),
        Command::ListMessages {
            email,
            folder,
            limit,
        } => stub(
            "list-messages",
            &format!("{email}:{folder} (limit={limit})"),
            "Phase 0 Week 4",
        ),
        Command::ShowMessage { id } => stub("show-message", &id, "Phase 0 Week 4"),
        Command::Sync { email } => stub("sync", &email, "Phase 0 Week 4"),
    }
}

fn stub(name: &str, arg: &str, when: &str) -> Result<(), MailcliError> {
    println!("mailcli: `{name} {arg}` is a Phase 0 stub; the real implementation lands in {when}.");
    Ok(())
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
