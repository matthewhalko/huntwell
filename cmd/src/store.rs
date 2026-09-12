//! Postgres persistence. Every function that reads or writes tenant data takes
//! the owning `account_id`, and every query filters by it: a bare `PlanId` is
//! never trusted, because an id is a guessable integer and a cross-tenant
//! read is one missing `WHERE` away.
//!
//! The schema itself lives in `local-infra/db/public/*.sql`; `build.rs`
//! concatenates it and [`migrate`] applies it at startup. Nothing in this
//! file creates a table.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::IpAddr;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Datelike, Duration, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::{FromRow, Row};

use crate::prospect::Prospect;

pub type Db = PgPool;

const SCHEMA: &str = include_str!(concat!(env!("OUT_DIR"), "/schema.sql"));
/// Per-database subsets, applied by the k3d migrate Jobs (see `migrate_schema`).
const SCHEMA_AUTH: &str = include_str!(concat!(env!("OUT_DIR"), "/schema.auth.sql"));
const SCHEMA_CORE: &str = include_str!(concat!(env!("OUT_DIR"), "/schema.core.sql"));

pub const PLAN_TYPE_BUSINESS: &str = "business";
pub const PLAN_TYPE_INDIVIDUAL: &str = "individual";

/// Per-plan caps on the search trail, trimmed after every scrape.
pub const MAX_QUERIES_PER_PLAN: i64 = 400;
pub const MAX_PAGES_PER_PLAN: i64 = 2_000;
/// Audit rows kept per account.
pub const AUDIT_KEEP_ROWS: i64 = 2_000;

/// Connect, creating the database first if the server says it does not exist.
///
/// The control plane is the first thing to start on a fresh server, so it is
/// the one that finds an empty Postgres. Normal starts pay nothing for this:
/// the extra connection happens only after a real `3D000` (invalid catalog
/// name), never on the happy path.
///
/// `CREATE DATABASE` needs a role with CREATEDB. If the deploy uses a
/// least-privilege role that lacks it, the error says so and names the
/// database, rather than reporting a permission failure with no context.
pub async fn connect_or_create(url: &str, max: u32) -> Result<Db> {
    match connect(url, max).await {
        Ok(db) => Ok(db),
        Err(e) if is_missing_database(&e) => {
            let name = create_database(url).await?;
            tracing::info!("created database {name}");
            connect(url, max).await
        }
        Err(e) => Err(e),
    }
}

/// `3D000` — invalid_catalog_name: the server answered, and the database in
/// the URL is not there. Anything else (bad host, bad password, TLS) must
/// surface unchanged, so we never try to create a database in response to a
/// connection problem that creating one cannot fix.
fn is_missing_database(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<sqlx::Error>(),
        Some(sqlx::Error::Database(db)) if db.code().as_deref() == Some("3D000")
    )
}

/// Connects to the `postgres` maintenance database on the same server and
/// creates the one the URL names. Returns the created name.
async fn create_database(url: &str) -> Result<String> {
    use sqlx::postgres::PgConnectOptions;
    use sqlx::{ConnectOptions, Executor};
    use std::str::FromStr;

    let opts = PgConnectOptions::from_str(url).with_context(|| format!("parse {}", redact(url)))?;
    let name = opts.get_database().unwrap_or_default().to_string();
    if name.is_empty() {
        bail!("{} names no database to create", redact(url));
    }

    // `postgres` is present on every server; `template1` is the fallback for
    // the rare cluster where it was dropped.
    let mut last: Option<sqlx::Error> = None;
    for maintenance in ["postgres", "template1"] {
        match opts.clone().database(maintenance).connect().await {
            Ok(mut conn) => {
                // Identifier, not a value — it cannot be a bind parameter, and
                // CREATE DATABASE cannot run inside a transaction. Quote it so
                // a name needing quoting works and one containing a quote
                // cannot break out.
                let stmt = format!("CREATE DATABASE \"{}\"", name.replace('"', "\"\""));
                return match conn.execute(stmt.as_str()).await {
                    Ok(_) => Ok(name),
                    // 42P04 — another process won the race. That is the
                    // outcome we wanted, so it is not an error.
                    Err(sqlx::Error::Database(db)) if db.code().as_deref() == Some("42P04") => Ok(name),
                    Err(e) => Err(anyhow::Error::new(e))
                        .with_context(|| format!("create database {name} (the role needs CREATEDB)")),
                };
            }
            Err(e) => last = Some(e),
        }
    }
    Err(anyhow::Error::new(last.expect("loop ran at least once")))
        .with_context(|| format!("connect to the maintenance database to create {name}"))
}

pub async fn connect(url: &str, max: u32) -> Result<Db> {
    PgPoolOptions::new()
        .max_connections(max)
        .connect(url)
        .await
        .with_context(|| format!("connect to {}", redact(url)))
}

/// Same pool, but every connection is opened read-only at the session level,
/// so the MCP server handed to a web-reading agent *cannot* write regardless
/// of what SQL an injected instruction talks it into.
pub async fn connect_read_only(url: &str) -> Result<Db> {
    use sqlx::Executor;
    PgPoolOptions::new()
        .max_connections(2)
        .after_connect(|conn, _| {
            Box::pin(async move {
                conn.execute("SET default_transaction_read_only = on").await?;
                Ok(())
            })
        })
        .connect(url)
        .await
        .with_context(|| format!("connect read-only to {}", redact(url)))
}

fn redact(url: &str) -> String {
    match (url.find("://"), url.rfind('@')) {
        (Some(a), Some(b)) if b > a => format!("{}://***@{}", &url[..a], &url[b + 1..]),
        _ => url.to_string(),
    }
}

/// Applies the schema. Every statement is `IF NOT EXISTS`, so this is safe on
/// every start; it is serialised with an advisory lock so two processes
/// starting together do not race on the same `CREATE`.
pub async fn migrate(db: &Db) -> Result<()> {
    migrate_sql(db, SCHEMA).await
}

/// Which slice of the schema to apply. `all` is the whole thing (one shared
/// database, as `serve`/local-infra use); `auth` and `core` are the per-database
/// subsets the hosted (k3d) migrate Jobs apply.
pub fn schema_for(which: &str) -> Result<&'static str> {
    match which {
        "all" => Ok(SCHEMA),
        "auth" => Ok(SCHEMA_AUTH),
        "core" => Ok(SCHEMA_CORE),
        other => anyhow::bail!("unknown schema {other:?} (expected all, auth or core)"),
    }
}

async fn migrate_sql(db: &Db, schema: &str) -> Result<()> {
    let mut conn = db.acquire().await?;
    sqlx::query("SELECT pg_advisory_lock(7411)").execute(&mut *conn).await?;
    let result = sqlx::raw_sql(schema).execute(&mut *conn).await;
    sqlx::query("SELECT pg_advisory_unlock(7411)").execute(&mut *conn).await?;
    result.context("apply schema")?;
    Ok(())
}

/// Applies one named schema slice to `db`. Used by `huntwell migrate`.
pub async fn migrate_schema(db: &Db, which: &str) -> Result<()> {
    migrate_sql(db, schema_for(which)?).await
}

pub fn normalize_plan_type(s: &str) -> String {
    if s.trim().eq_ignore_ascii_case(PLAN_TYPE_INDIVIDUAL) {
        PLAN_TYPE_INDIVIDUAL.to_string()
    } else {
        PLAN_TYPE_BUSINESS.to_string()
    }
}

pub fn is_individual(plan_type: &str) -> bool {
    plan_type.trim().eq_ignore_ascii_case(PLAN_TYPE_INDIVIDUAL)
}

pub fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

fn ts(t: Option<DateTime<Utc>>) -> String {
    t.map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Accounts and sessions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Account {
    pub account_id: i64,
    pub email: String,
    pub display_name: String,
    #[serde(skip)]
    /// Empty under Cognito identity — the pool holds the password there.
    pub password_hash: String,
    /// The Cognito subject this account is, empty on a local-identity install.
    pub cognito_sub: String,
    pub timezone: String,
    /// True while the timezone follows the browser. Set false the moment
    /// somebody picks one in Settings.
    pub timezone_auto: bool,
    pub theme: String,
    pub created_at: DateTime<Utc>,
    /// When first-run setup was completed. `None` until it is, which is what
    /// the UI shows the setup dialog on.
    pub onboarded_at: Option<DateTime<Utc>>,
    /// The workspace this person is working in, when it is not their own.
    /// Resolved and membership-checked at authentication time.
    pub active_workspace_id: Option<i64>,
    /// What this account's workspace is called. Empty until somebody names it.
    pub workspace_name: String,
    /// Plan kinds this account may create, comma-separated. Empty defers to the
    /// installation default — see [`allowed_kinds`].
    pub enabled_kinds: String,
    /// When this workspace claimed the right to collect from the restricted
    /// platforms. `None` means runs refuse those hosts.
    pub platform_ack_at: Option<DateTime<Utc>>,
    /// The person who accepted, and the wording they accepted.
    pub platform_ack_by: Option<i64>,
    pub platform_ack_text: String,
    /// May this workspace connect an authenticated session? Off unless an
    /// operator turned it on — see the column comment in account.sql.
    pub connected_logins: bool,
}

impl Account {
    /// The tenant every data query filters by: the workspace being worked in.
    /// Their own account unless they have switched into a shared one — which
    /// is why nothing downstream of authentication knows teams exist.
    pub fn tenant(&self) -> i64 {
        self.active_workspace_id.unwrap_or(self.account_id)
    }

    /// True when they are working in their own workspace.
    pub fn in_own_workspace(&self) -> bool {
        self.tenant() == self.account_id
    }
}

const ACCOUNT_COLS: &str =
    r#"account_id,email,display_name,password_hash,cognito_sub,timezone,timezone_auto,theme,created_at,onboarded_at,active_workspace_id,workspace_name,enabled_kinds,platform_ack_at,platform_ack_by,platform_ack_text,connected_logins"#;

pub async fn create_account(db: &Db, email: &str, display_name: &str, id: &crate::identity::NewIdentity) -> Result<Account> {
    let email = email.trim().to_lowercase();
    let row = sqlx::query(&format!(
        r#"INSERT INTO account (email,display_name,password_hash,cognito_sub) VALUES ($1,$2,$3,$4)
           RETURNING {ACCOUNT_COLS}"#
    ))
    .bind(&email)
    .bind(display_name.trim())
    .bind(&id.password_hash)
    .bind(&id.cognito_sub)
    .fetch_one(db)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(ref d) if d.is_unique_violation() => anyhow!("an account with that email already exists"),
        e => anyhow!(e),
    })?;
    Ok(Account::from_row(&row)?)
}

/// Record the Cognito subject on an account that did not have one.
///
/// Written on a successful sign-in rather than by a migration: matching a row
/// to a pool user by email address alone is a guess, and a wrong guess hands
/// one person another's workspace.
pub async fn set_cognito_sub(db: &Db, account_id: i64, sub: &str) -> Result<()> {
    sqlx::query(r#"UPDATE account SET cognito_sub=$2 WHERE account_id=$1"#)
        .bind(account_id)
        .bind(sub)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn account_count(db: &Db) -> Result<i64> {
    Ok(sqlx::query_scalar(r#"SELECT count(*) FROM account"#).fetch_one(db).await?)
}

pub async fn find_account_by_email(db: &Db, email: &str) -> Result<Option<Account>> {
    let row = sqlx::query(&format!(r#"SELECT {ACCOUNT_COLS} FROM account WHERE email = $1"#))
        .bind(email.trim().to_lowercase())
        .fetch_optional(db)
        .await?;
    row.map(|r| Account::from_row(&r).map_err(Into::into)).transpose()
}

pub async fn get_account(db: &Db, account_id: i64) -> Result<Option<Account>> {
    let row = sqlx::query(&format!(r#"SELECT {ACCOUNT_COLS} FROM account WHERE account_id = $1"#))
        .bind(account_id)
        .fetch_optional(db)
        .await?;
    row.map(|r| Account::from_row(&r).map_err(Into::into)).transpose()
}

// ---------------------------------------------------------------------------
// Teams: membership of a workspace, and the invitations that create it
// ---------------------------------------------------------------------------

/// A person with access to a workspace — the owner, or someone invited in.
#[derive(Debug, Clone, Serialize)]
pub struct Member {
    pub account_id: i64,
    pub email: String,
    pub display_name: String,
    pub role: String,
    pub joined_at: String,
    /// True for the account the workspace belongs to.
    pub owner: bool,
}

/// A workspace someone can work in: their own, plus any they were invited to.
#[derive(Debug, Clone, Serialize)]
pub struct Workspace {
    pub workspace_id: i64,
    pub name: String,
    pub role: String,
    pub own: bool,
}

/// An invitation that has not been accepted yet.
#[derive(Debug, Clone, Serialize)]
pub struct InviteRow {
    pub invite_id: i64,
    pub email: String,
    pub role: String,
    pub token: String,
    pub created_at: String,
    pub expires_at: String,
}

/// What to call a workspace: its own name, else whose it is. One SQL
/// expression so the switcher, the team list and the invite email cannot
/// disagree about what a workspace is called.
const WORKSPACE_NAME: &str =
    r#"COALESCE(NULLIF(a.workspace_name,''), NULLIF(a.display_name,'') || '''s workspace', a.email)"#;

/// Names the workspace an account owns. Empty clears it back to the default.
pub async fn set_workspace_name(db: &Db, workspace_id: i64, name: &str) -> Result<()> {
    sqlx::query(r#"UPDATE account SET workspace_name=$2 WHERE account_id=$1"#)
        .bind(workspace_id)
        .bind(name.trim().chars().take(120).collect::<String>())
        .execute(db)
        .await?;
    Ok(())
}

/// A workspace's name, however it is derived.
pub async fn workspace_name(db: &Db, workspace_id: i64) -> Result<String> {
    let row = sqlx::query(&format!(r#"SELECT {WORKSPACE_NAME} FROM account a WHERE a.account_id=$1"#))
        .bind(workspace_id)
        .fetch_optional(db)
        .await?;
    Ok(row.map(|r| r.get::<String, _>(0)).unwrap_or_default())
}

/// A random, URL-safe secret: an invitation's whole authorisation. Same shape
/// and strength as a session token (192 bits, hex).
pub fn random_token() -> String {
    let mut b = [0u8; 24];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut b);
    hex::encode(b)
}

/// Whether `member` may work in `workspace`, and as what. The workspace's own
/// account is always its owner; everyone else needs a row.
pub async fn workspace_role(db: &Db, workspace_id: i64, member_id: i64) -> Result<Option<String>> {
    if workspace_id == member_id {
        return Ok(Some("owner".into()));
    }
    let row = sqlx::query(r#"SELECT role FROM membership WHERE workspace_id=$1 AND member_id=$2"#)
        .bind(workspace_id)
        .bind(member_id)
        .fetch_optional(db)
        .await?;
    Ok(row.map(|r| r.get::<String, _>(0)))
}

/// Everyone who can open this workspace, owner first.
pub async fn list_members(db: &Db, workspace_id: i64) -> Result<Vec<Member>> {
    let rows = sqlx::query(
        r#"SELECT a.account_id, a.email, a.display_name, 'owner' AS role, a.created_at, true AS owner
             FROM account a WHERE a.account_id = $1
           UNION ALL
           SELECT a.account_id, a.email, a.display_name, m.role, m.created_at, false
             FROM membership m JOIN account a ON a.account_id = m.member_id
            WHERE m.workspace_id = $1
           ORDER BY 6 DESC, 5"#,
    )
    .bind(workspace_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| Member {
            account_id: r.get(0),
            email: r.get(1),
            display_name: r.get(2),
            role: r.get(3),
            joined_at: r.get::<DateTime<Utc>, _>(4).to_rfc3339(),
            owner: r.get(5),
        })
        .collect())
}

/// The workspaces this person can switch between.
pub async fn list_workspaces(db: &Db, account_id: i64) -> Result<Vec<Workspace>> {
    let own = sqlx::query(&format!(r#"SELECT {WORKSPACE_NAME} FROM account a WHERE a.account_id=$1"#))
        .bind(account_id)
        .fetch_optional(db)
        .await?;
    let mut out = Vec::new();
    if let Some(r) = own {
        out.push(Workspace { workspace_id: account_id, name: r.get(0), role: "owner".into(), own: true });
    }
    let rows = sqlx::query(&format!(
        r#"SELECT a.account_id, {WORKSPACE_NAME}, m.role
             FROM membership m JOIN account a ON a.account_id = m.workspace_id
            WHERE m.member_id = $1 ORDER BY m.created_at"#
    ))
    .bind(account_id)
    .fetch_all(db)
    .await?;
    for r in rows {
        out.push(Workspace { workspace_id: r.get(0), name: r.get(1), role: r.get(2), own: false });
    }
    Ok(out)
}

/// Switches which workspace a person is working in. `None` — or their own id —
/// puts them back in their own.
pub async fn set_active_workspace(db: &Db, account_id: i64, workspace_id: Option<i64>) -> Result<()> {
    let w = workspace_id.filter(|w| *w != account_id);
    sqlx::query(r#"UPDATE account SET active_workspace_id=$2 WHERE account_id=$1"#)
        .bind(account_id)
        .bind(w)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn add_member(db: &Db, workspace_id: i64, member_id: i64, role: &str, invited_by: i64) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO membership (workspace_id,member_id,role,invited_by_id) VALUES ($1,$2,$3,$4)
           ON CONFLICT (workspace_id,member_id) DO UPDATE SET role=EXCLUDED.role"#,
    )
    .bind(workspace_id)
    .bind(member_id)
    .bind(role)
    .bind(invited_by)
    .execute(db)
    .await?;
    Ok(())
}

/// Removes someone from a workspace, and puts them back in their own if they
/// were working in it — otherwise their next request would be scoped to a
/// workspace they no longer belong to.
pub async fn remove_member(db: &Db, workspace_id: i64, member_id: i64) -> Result<bool> {
    let n = sqlx::query(r#"DELETE FROM membership WHERE workspace_id=$1 AND member_id=$2"#)
        .bind(workspace_id)
        .bind(member_id)
        .execute(db)
        .await?
        .rows_affected();
    sqlx::query(r#"UPDATE account SET active_workspace_id=NULL WHERE account_id=$1 AND active_workspace_id=$2"#)
        .bind(member_id)
        .bind(workspace_id)
        .execute(db)
        .await?;
    Ok(n > 0)
}

pub async fn create_invite(db: &Db, workspace_id: i64, email: &str, role: &str, invited_by: i64, token: &str) -> Result<InviteRow> {
    let email = email.trim().to_lowercase();
    // One live invite per address per workspace: re-inviting refreshes the
    // token rather than leaving two valid links in the world.
    sqlx::query(r#"DELETE FROM invite WHERE workspace_id=$1 AND email=$2 AND accepted_at IS NULL"#)
        .bind(workspace_id)
        .bind(&email)
        .execute(db)
        .await?;
    let row = sqlx::query(
        r#"INSERT INTO invite (workspace_id,email,role,token,invited_by_id)
           VALUES ($1,$2,$3,$4,$5)
           RETURNING invite_id,email,role,token,created_at,expires_at"#,
    )
    .bind(workspace_id)
    .bind(&email)
    .bind(role)
    .bind(token)
    .bind(invited_by)
    .fetch_one(db)
    .await?;
    Ok(InviteRow {
        invite_id: row.get(0),
        email: row.get(1),
        role: row.get(2),
        token: row.get(3),
        created_at: row.get::<DateTime<Utc>, _>(4).to_rfc3339(),
        expires_at: row.get::<DateTime<Utc>, _>(5).to_rfc3339(),
    })
}

pub async fn list_invites(db: &Db, workspace_id: i64) -> Result<Vec<InviteRow>> {
    let rows = sqlx::query(
        r#"SELECT invite_id,email,role,token,created_at,expires_at FROM invite
           WHERE workspace_id=$1 AND accepted_at IS NULL AND expires_at > now() ORDER BY created_at DESC"#,
    )
    .bind(workspace_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| InviteRow {
            invite_id: r.get(0),
            email: r.get(1),
            role: r.get(2),
            token: r.get(3),
            created_at: r.get::<DateTime<Utc>, _>(4).to_rfc3339(),
            expires_at: r.get::<DateTime<Utc>, _>(5).to_rfc3339(),
        })
        .collect())
}

pub async fn revoke_invite(db: &Db, workspace_id: i64, invite_id: i64) -> Result<bool> {
    let n = sqlx::query(r#"DELETE FROM invite WHERE workspace_id=$1 AND invite_id=$2 AND accepted_at IS NULL"#)
        .bind(workspace_id)
        .bind(invite_id)
        .execute(db)
        .await?
        .rows_affected();
    Ok(n > 0)
}

/// A live invitation, by token: unaccepted, unexpired, with the workspace's
/// name for the "join X" screen.
pub async fn invite_by_token(db: &Db, token: &str) -> Result<Option<(i64, String, String, String)>> {
    let row = sqlx::query(
        &format!(
            r#"SELECT i.workspace_id, i.email, i.role, {WORKSPACE_NAME}
                 FROM invite i JOIN account a ON a.account_id = i.workspace_id
                WHERE i.token=$1 AND i.accepted_at IS NULL AND i.expires_at > now()"#
        ),
    )
    .bind(token)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| (r.get(0), r.get(1), r.get(2), r.get(3))))
}

/// Marks an invitation used. Single-use by construction: the row can only go
/// from unaccepted to accepted once.
pub async fn accept_invite(db: &Db, token: &str, account_id: i64) -> Result<bool> {
    let n = sqlx::query(
        r#"UPDATE invite SET accepted_at=now(), accepted_by_id=$2
           WHERE token=$1 AND accepted_at IS NULL AND expires_at > now()"#,
    )
    .bind(token)
    .bind(account_id)
    .execute(db)
    .await?
    .rows_affected();
    Ok(n > 0)
}

// ---------------------------------------------------------------------------
// The knowledge graph: what led to what, and what each angle cost
// ---------------------------------------------------------------------------

/// One edge of a plan's graph, as the canvas draws it.
#[derive(Debug, Clone, Serialize)]
pub struct GraphEdge {
    pub from_kind: String,
    pub from: String,
    pub to_kind: String,
    pub to: String,
    pub weight: i32,
    /// Where this edge falls in the plan's traversal — what the path is drawn
    /// along, and what colours it.
    pub seq: i64,
}

/// Records that `from` led to `to`. Seeing it again thickens the edge rather
/// than adding another one.
pub async fn record_edge(
    db: &Db,
    plan_id: i64,
    execution_id: Option<i64>,
    from_kind: &str,
    from_key: &str,
    to_kind: &str,
    to_key: &str,
) -> Result<()> {
    if from_key.trim().is_empty() || to_key.trim().is_empty() {
        return Ok(());
    }
    sqlx::query(
        // Seq is the plan's own traversal counter: this is the nth way it went.
        // An edge walked again keeps the place it first had.
        r#"INSERT INTO trail_edge (plan_id,from_kind,from_key,to_kind,to_key,execution_id,seq)
           VALUES ($1,$2,$3,$4,$5,$6,
                   (SELECT COALESCE(MAX(seq),0) + 1 FROM trail_edge WHERE plan_id=$1))
           ON CONFLICT (plan_id,from_kind,from_key,to_kind,to_key)
           DO UPDATE SET weight = trail_edge.weight + 1, last_seen_at = now()"#,
    )
    .bind(plan_id)
    .bind(from_kind)
    .bind(from_key)
    .bind(to_kind)
    .bind(to_key)
    .bind(execution_id)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn list_edges(db: &Db, plan_id: i64, limit: i64) -> Result<Vec<GraphEdge>> {
    let rows = sqlx::query(
        // In traversal order: the graph is a path before it is a picture.
        r#"SELECT from_kind,from_key,to_kind,to_key,weight,seq FROM trail_edge
           WHERE plan_id=$1 ORDER BY seq, created_at LIMIT $2"#,
    )
    .bind(plan_id)
    .bind(limit.clamp(1, 5000))
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| GraphEdge {
            from_kind: r.get(0),
            from: r.get(1),
            to_kind: r.get(2),
            to: r.get(3),
            weight: r.get(4),
            seq: r.get(5),
        })
        .collect())
}

/// The results a plan has stored, as graph nodes: whatever kind it collects.
#[derive(Debug, Clone, Serialize)]
pub struct ResultNode {
    pub key: String,
    pub label: String,
    pub sub: String,
}

pub async fn list_result_nodes(db: &Db, plan_id: i64, limit: i64) -> Result<Vec<ResultNode>> {
    let rows = sqlx::query(
        r#"SELECT source_key, COALESCE(NULLIF(company,''), NULLIF(name,''), source_key), COALESCE(name,'')
             FROM prospect WHERE plan_id=$1 ORDER BY last_seen_utc DESC LIMIT $2"#,
    )
    .bind(plan_id)
    .bind(limit.clamp(1, 2000))
    .fetch_all(db)
    .await?;
    if !rows.is_empty() {
        return Ok(rows
            .iter()
            .map(|r| ResultNode { key: r.get(0), label: r.get(1), sub: r.get(2) })
            .collect());
    }
    // Artifact plans keep their rows elsewhere; a graph of an empty prospects
    // table would otherwise look like a plan that found nothing.
    let rows = sqlx::query(
        r#"SELECT source_key, COALESCE(NULLIF(title,''), source_key), ''
             FROM artifact WHERE plan_id=$1 ORDER BY last_seen_utc DESC LIMIT $2"#,
    )
    .bind(plan_id)
    .bind(limit.clamp(1, 2000))
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| ResultNode { key: r.get(0), label: r.get(1), sub: r.get(2) })
        .collect())
}

/// Credits the queries an iteration used with what it produced and what it
/// cost. Approximate by construction — the spend is split evenly across the
/// angles that were in play — and labelled as such wherever it is shown.
pub async fn attribute_query_yield(db: &Db, plan_id: i64, keys: &[String], new_rows: i64, tokens: i64) -> Result<()> {
    attribute_query_yield_inner(db, plan_id, keys, new_rows).await?;
    if keys.is_empty() || tokens <= 0 {
        return Ok(());
    }
    let each = tokens / keys.len() as i64;
    for k in keys {
        sqlx::query(r#"UPDATE search_query SET tokens=tokens+$3 WHERE plan_id=$1 AND query_key=$2"#)
            .bind(plan_id)
            .bind(k)
            .bind(each)
            .execute(db)
            .await?;
    }
    Ok(())
}

/// Stamps first-run setup as finished. Idempotent: the first stamp wins, so a
/// user who reopens the dialog does not rewrite their own start date.
pub async fn mark_onboarded(db: &Db, account_id: i64) -> Result<()> {
    sqlx::query(r#"UPDATE account SET onboarded_at=now() WHERE account_id=$1 AND onboarded_at IS NULL"#)
        .bind(account_id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn update_account_prefs(db: &Db, account_id: i64, display_name: &str, timezone: &str, tz_auto: bool, theme: &str) -> Result<()> {
    sqlx::query(r#"UPDATE account SET display_name=$2, timezone=$3, timezone_auto=$4, theme=$5 WHERE account_id=$1"#)
        .bind(account_id)
        .bind(display_name.trim())
        .bind(timezone.trim())
        .bind(tz_auto)
        .bind(theme.trim())
        .execute(db)
        .await?;
    Ok(())
}

pub async fn update_password(db: &Db, account_id: i64, password_hash: &str) -> Result<()> {
    sqlx::query(r#"UPDATE account SET password_hash=$2 WHERE account_id=$1"#)
        .bind(account_id)
        .bind(password_hash)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn create_session(db: &Db, account_id: i64, token_hash: &str, ttl_hours: i64, user_agent: &str) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO session (token_hash,account_id,expires_at,user_agent)
           VALUES ($1,$2, now() + make_interval(hours => $3), $4)"#,
    )
    .bind(token_hash)
    .bind(account_id)
    .bind(ttl_hours as i32)
    .bind(user_agent.chars().take(400).collect::<String>())
    .execute(db)
    .await?;
    sqlx::query(r#"UPDATE account SET last_login_at = now() WHERE account_id = $1"#)
        .bind(account_id)
        .execute(db)
        .await?;
    Ok(())
}

/// The account a live session belongs to, touching LastSeenAt on the way.
pub async fn session_account(db: &Db, token_hash: &str) -> Result<Option<Account>> {
    let row = sqlx::query(&format!(
        r#"UPDATE session s SET last_seen_at = now()
           FROM account a
           WHERE s.token_hash = $1 AND s.expires_at > now() AND a.account_id = s.account_id
           RETURNING a.account_id, a.email, a.display_name, a.password_hash, a.cognito_sub, a.timezone, a.timezone_auto, a.theme, a.created_at, a.onboarded_at, a.active_workspace_id, a.workspace_name, a.enabled_kinds, a.platform_ack_at, a.platform_ack_by, a.platform_ack_text, a.connected_logins"#
    ))
    .bind(token_hash)
    .fetch_optional(db)
    .await?;
    row.map(|r| Account::from_row(&r).map_err(Into::into)).transpose()
}

pub async fn delete_session(db: &Db, token_hash: &str) -> Result<()> {
    sqlx::query(r#"DELETE FROM session WHERE token_hash = $1"#).bind(token_hash).execute(db).await?;
    Ok(())
}

pub async fn delete_expired_sessions(db: &Db) -> Result<u64> {
    Ok(sqlx::query(r#"DELETE FROM session WHERE expires_at <= now()"#).execute(db).await?.rows_affected())
}

// ---------------------------------------------------------------------------
// Plans (the original SourceConfig)
// ---------------------------------------------------------------------------

/// A plan, as the API and the pipeline see it. Serde names are PascalCase so
/// the prompt-authoring code and the UI's field names match the original.
#[derive(Debug, Clone, Default, Serialize, Deserialize, FromRow)]
pub struct SourceConfig {
    #[serde(rename = "PlanId", default)]
    pub plan_id: i64,
    #[serde(skip)]
    pub account_id: i64,
    #[serde(rename = "Source")]
    pub source: String,
    /// The brief in the user's own words. The only prose about a plan the UI
    /// shows — the prompts behind it are never sent to a browser.
    #[serde(rename = "Description", default)]
    pub description: String,
    #[serde(rename = "PlanType", default)]
    pub plan_type: String,
    /// What this plan produces: 'prospects' (default; empty means prospects),
    /// 'artifacts' (custom rows), 'report' (one document) or 'assets' (files).
    #[serde(rename = "Kind", default)]
    pub kind: String,
    /// For 'artifacts' plans: JSON array of {key,label,type,role} column specs.
    #[serde(rename = "FieldsSchemaJson", default)]
    pub fields_schema_json: String,
    /// For 'report' and 'assets' plans: what the run is about (one company,
    /// person or topic). Unused by the row kinds.
    #[serde(rename = "Subject", default)]
    pub subject: String,
    #[serde(rename = "ScrapePrompt")]
    pub scrape_prompt: String,
    #[serde(rename = "EnrichPrompt", default)]
    pub enrich_prompt: String,
    #[serde(rename = "PlannerPrompt", default)]
    pub planner_prompt: String,
    #[serde(rename = "SourceKeyTmpl", default)]
    pub source_key_tmpl: String,
    #[serde(rename = "NameTmpl", default)]
    pub name_tmpl: String,
    #[serde(rename = "TitleTmpl", default)]
    pub title_tmpl: String,
    #[serde(rename = "CompanyTmpl", default)]
    pub company_tmpl: String,
    #[serde(rename = "IndustryTmpl", default)]
    pub industry_tmpl: String,
    #[serde(rename = "EmailTmpl", default)]
    pub email_tmpl: String,
    #[serde(rename = "EmailStatusTmpl", default)]
    pub email_status_tmpl: String,
    #[serde(rename = "PhoneTmpl", default)]
    pub phone_tmpl: String,
    #[serde(rename = "WebsiteTmpl", default)]
    pub website_tmpl: String,
    #[serde(rename = "LinkedInTmpl", default)]
    pub linkedin_tmpl: String,
    #[serde(rename = "LocationTmpl", default)]
    pub location_tmpl: String,
    #[serde(rename = "NotesTmpl", default)]
    pub notes_tmpl: String,
    #[serde(rename = "EstimatedValueTmpl", default)]
    pub estimated_value_tmpl: String,
    #[serde(rename = "MinValue", default)]
    pub min_value: i64,
    #[serde(rename = "Learn", default)]
    pub learn: bool,
    #[serde(rename = "Iterations", default = "one")]
    pub iterations: i32,
    #[serde(rename = "MaxNoProgress", default = "two")]
    pub max_no_progress: i32,
    #[serde(rename = "KnownLimit", default = "sixty")]
    pub known_limit: i32,
    /// 'drafting' | 'ready' | 'failed' — see the column comment. Serialized so
    /// the UI can show a plan being built.
    #[serde(rename = "DraftStatus", default)]
    pub draft_status: String,
    /// How hard a run tries: 'quick' | 'normal' | 'thorough' | 'exhaustive'.
    /// The iteration count and the zero-yield limit are derived from it.
    #[serde(rename = "Effort", default)]
    pub effort: String,
    #[serde(rename = "TargetProspects", default)]
    pub target_prospects: i32,
    #[serde(rename = "FreeAgent", default)]
    pub free_agent: bool,
    #[serde(rename = "Favorite", default)]
    pub favorite: bool,
    #[serde(rename = "Model", default)]
    pub model: String,
    /// Per-stage Cursor `--model` ids. Empty falls back to `model`, then the
    /// install default. Search / research / find-files share scrape.
    #[serde(rename = "ModelScrape", default)]
    pub model_scrape: String,
    #[serde(rename = "ModelEnrich", default)]
    pub model_enrich: String,
    #[serde(rename = "ModelPlanner", default)]
    pub model_planner: String,
    #[serde(rename = "SeedVarsJSON", default = "empty_obj")]
    pub seed_vars_json: String,
    #[serde(rename = "AllowHosts", default)]
    pub allow_hosts: String,
    /// Websites to search first, comma-separated. Steering, not containment:
    /// see `allow_hosts` for the fence that ends a run.
    #[serde(rename = "Sites", default)]
    pub sites: String,
    /// Email the owner when a run finds rows this plan had not seen before.
    #[serde(rename = "AlertEmail", default)]
    pub alert_email: bool,
    #[serde(rename = "ScheduleEnabled", default)]
    pub schedule_enabled: bool,
    #[serde(rename = "ScheduleTime", default)]
    pub schedule_time: String,
    #[serde(rename = "ScheduleDays", default)]
    pub schedule_days: String,
    /// Derived; server-owned.
    #[serde(rename = "NextRunAt", default, skip_deserializing)]
    pub next_run_at: Option<DateTime<Utc>>,
    #[serde(rename = "LastScheduledAt", default, skip_deserializing)]
    pub last_scheduled_at: Option<DateTime<Utc>>,
    #[serde(rename = "UpdatedAt", default, skip_deserializing)]
    pub updated_at: Option<DateTime<Utc>>,
}

fn one() -> i32 {
    1
}
fn two() -> i32 {
    2
}
/// The exclusion sample replayed into a scrape prompt. Short on purpose: the
/// agent's `prospect_known` tool screens a whole results page against the full
/// record in one call, so a long inlined list is a second copy nobody reads.
fn sixty() -> i32 {
    60
}
fn empty_obj() -> String {
    "{}".into()
}

/// What a plan produces. The stored `Kind` string maps here; anything
/// unrecognised (including empty) is the original default, prospects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanKind {
    /// People or companies to contact.
    Prospects,
    /// Rows with a user-defined column schema.
    Artifacts,
    /// One synthesized narrative document about a subject.
    Report,
    /// Files found about a subject, kept in the object store.
    Assets,
}

impl PlanKind {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "artifacts" => Self::Artifacts,
            "report" => Self::Report,
            "assets" => Self::Assets,
            _ => Self::Prospects,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Prospects => "prospects",
            Self::Artifacts => "artifacts",
            Self::Report => "report",
            Self::Assets => "assets",
        }
    }
}

impl SourceConfig {
    pub fn audience(&self) -> String {
        normalize_plan_type(&self.plan_type)
    }
    pub fn targets_individuals(&self) -> bool {
        is_individual(&self.plan_type)
    }
    pub fn kind_of(&self) -> PlanKind {
        PlanKind::parse(&self.kind)
    }
    /// The model this plan wants for one pipeline stage, if any.
    ///
    /// The per-stage column wins; the legacy all-stages `model` is the
    /// fallback so an old row still means what it used to. `None` leaves the
    /// choice to the install default, then to Cursor.
    pub fn stage_model(&self, stage: &str) -> Option<String> {
        let raw = match stage {
            "scrape" => self.model_scrape.as_str(),
            "enrich" => self.model_enrich.as_str(),
            "planner" => self.model_planner.as_str(),
            _ => "",
        };
        crate::agent::normalize_model(raw).or_else(|| crate::agent::normalize_model(&self.model))
    }
    /// True for custom-artifact plans (empty kind means the default, prospects).
    pub fn is_artifacts(&self) -> bool {
        self.kind_of() == PlanKind::Artifacts
    }
    pub fn seed_vars(&self) -> BTreeMap<String, Value> {
        serde_json::from_str::<BTreeMap<String, Value>>(&self.seed_vars_json).unwrap_or_default()
    }
}

/// The one definition of "this plan can run", so the API, the runner, the
/// pipeline and the UI cannot drift apart. Returns the reason it cannot.
///
/// Deliberately an exhaustive match: a kind that gains a requirement fails
/// loudly here instead of silently falling through to the prospects rules.
pub fn plan_ready(sc: &SourceConfig) -> Result<(), String> {
    if sc.scrape_prompt.trim().is_empty() {
        return Err("a scrape prompt is required".into());
    }
    match sc.kind_of() {
        PlanKind::Prospects => {
            if sc.source_key_tmpl.trim().is_empty() {
                return Err("a source key template is required — it is the dedupe key".into());
            }
        }
        PlanKind::Artifacts => {
            if crate::artifact::parse_schema(&sc.fields_schema_json).is_empty() {
                return Err("a custom-artifact plan needs at least one column".into());
            }
        }
        // Report and assets are about one subject; the subject is the dedupe
        // key and the thing the research prompt is pointed at.
        PlanKind::Report | PlanKind::Assets => {
            if sc.subject.trim().is_empty() {
                return Err("a subject is required — what the run is about".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod plan_model_tests {
    use super::*;

    #[test]
    fn a_stage_column_wins_over_the_legacy_all_stages_model() {
        let sc = SourceConfig {
            model: "composer-2.5".into(),
            model_scrape: "claude-sonnet-5".into(),
            ..Default::default()
        };
        assert_eq!(sc.stage_model("scrape").as_deref(), Some("claude-sonnet-5"));
        assert_eq!(sc.stage_model("enrich").as_deref(), Some("composer-2.5"));
        assert_eq!(sc.stage_model("planner").as_deref(), Some("composer-2.5"));
    }

    #[test]
    fn empty_means_the_install_default_not_a_literal() {
        let sc = SourceConfig::default();
        assert_eq!(sc.stage_model("scrape"), None);
    }
}

const PLAN_COLS: &str = r#"plan_id,account_id,source,description,plan_type,kind,fields_schema_json,subject,scrape_prompt,enrich_prompt,planner_prompt,
source_key_tmpl,name_tmpl,title_tmpl,company_tmpl,industry_tmpl,email_tmpl,email_status_tmpl,phone_tmpl,
website_tmpl,linkedin_tmpl,location_tmpl,notes_tmpl,estimated_value_tmpl,min_value,learn,iterations,max_no_progress,known_limit,
target_prospects,effort,draft_status,free_agent,favorite,model,model_scrape,model_enrich,model_planner,seed_vars_json,allow_hosts,sites,alert_email,schedule_enabled,schedule_time,
schedule_days,next_run_at,last_scheduled_at,updated_at"#;

/// A plan plus the counts the plans grid shows.
#[derive(Debug, Clone, Serialize)]
pub struct PlanSummary {
    #[serde(flatten)]
    pub plan: SourceConfig,
    pub prospects: i64,
    pub executions: i64,
    /// Billable tokens this plan has spent across every run.
    pub tokens: i64,
    pub last_execution_at: String,
    pub last_execution_status: String,
    pub active_execution_id: Option<i64>,
}

pub async fn list_plans(db: &Db, account_id: i64) -> Result<Vec<PlanSummary>> {
    let rows = sqlx::query(&format!(
        // "prospects" is the plan card's result count: whatever this plan's
        // kind actually produces, so report and file plans don't read as empty.
        r#"SELECT {PLAN_COLS},
             (CASE p.kind
                WHEN 'artifacts' THEN (SELECT count(*) FROM artifact a WHERE a.plan_id = p.plan_id)
                WHEN 'report'    THEN (SELECT count(*) FROM report rp WHERE rp.plan_id = p.plan_id)
                WHEN 'assets'    THEN (SELECT count(*) FROM asset f WHERE f.plan_id = p.plan_id)
                ELSE (SELECT count(*) FROM prospect pr WHERE pr.plan_id = p.plan_id)
              END) AS prospects,
             (SELECT count(*) FROM execution r WHERE r.plan_id = p.plan_id) AS executions,
             -- What this plan has cost so far, so "getting expensive" can be
             -- read against its own history rather than in the abstract.
             -- ::bigint because SUM over bigint answers NUMERIC in Postgres,
             -- which does not decode into an i64.
             (SELECT COALESCE(SUM(r.input_tokens + r.output_tokens), 0)::bigint FROM execution r WHERE r.plan_id = p.plan_id) AS tokens,
             (SELECT r.started_at FROM execution r WHERE r.plan_id = p.plan_id ORDER BY r.execution_id DESC LIMIT 1) AS last_execution_at,
             (SELECT r.status FROM execution r WHERE r.plan_id = p.plan_id ORDER BY r.execution_id DESC LIMIT 1) AS last_execution_status,
             (SELECT r.execution_id FROM execution r WHERE r.plan_id = p.plan_id AND r.status IN ('queued','running') ORDER BY r.execution_id DESC LIMIT 1) AS active_execution_id
           FROM plan p WHERE account_id = $1 ORDER BY favorite DESC, lower(source)"#
    ))
    .bind(account_id)
    .fetch_all(db)
    .await?;
    rows.iter()
        .map(|r| {
            Ok(PlanSummary {
                plan: SourceConfig::from_row(r)?,
                prospects: r.try_get("prospects")?,
                executions: r.try_get("executions")?,
                tokens: r.try_get("tokens")?,
                last_execution_at: ts(r.try_get("last_execution_at")?),
                last_execution_status: r.try_get::<Option<String>, _>("last_execution_status")?.unwrap_or_default(),
                active_execution_id: r.try_get("active_execution_id")?,
            })
        })
        .collect()
}

pub async fn get_plan(db: &Db, account_id: i64, plan_id: i64) -> Result<Option<SourceConfig>> {
    let row = sqlx::query(&format!(r#"SELECT {PLAN_COLS} FROM plan WHERE account_id = $1 AND plan_id = $2"#))
        .bind(account_id)
        .bind(plan_id)
        .fetch_optional(db)
        .await?;
    row.map(|r| SourceConfig::from_row(&r).map_err(Into::into)).transpose()
}

/// For the runner, which is handed a run id and derives the account from it.
pub async fn get_plan_unscoped(db: &Db, plan_id: i64) -> Result<Option<SourceConfig>> {
    let row = sqlx::query(&format!(r#"SELECT {PLAN_COLS} FROM plan WHERE plan_id = $1"#))
        .bind(plan_id)
        .fetch_optional(db)
        .await?;
    row.map(|r| SourceConfig::from_row(&r).map_err(Into::into)).transpose()
}

/// Where a plan is in being built. Unknown reads as 'ready', so a plan that
/// predates this column behaves as the finished thing it is.
pub fn normalize_draft_status(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        // Waiting for the planning service to pick it up. The UI treats this
        // exactly like "drafting" — from the user's side the plan is being
        // built; the difference is only which process is holding it.
        "queued" => "queued",
        "drafting" => "drafting",
        "failed" => "failed",
        _ => "ready",
    }
    .to_string()
}

/// Is this plan being built — queued for the planning service, or in its hands?
pub fn draft_in_progress(status: &str) -> bool {
    matches!(normalize_draft_status(status).as_str(), "queued" | "drafting")
}

/// The status as the outside world sees it.
///
/// 'queued' collapses to 'drafting': the difference is which process is holding
/// the plan, which is ours to know and no use to a caller. Keeping it internal
/// means the UI's polling and the documented `/v1` contract are unchanged by
/// whether a planning service is deployed.
/// Queue an email. This is what callers use instead of sending inline: it is a
/// single INSERT, it cannot fail because a provider is slow, and the row is the
/// record that the message is owed.
pub async fn queue_mail(
    db: &Db,
    account_id: Option<i64>,
    to: &str,
    kind: &str,
    email: &crate::mail::Email,
) -> Result<i64> {
    let id: i64 = sqlx::query_scalar(
        r#"INSERT INTO mail_outbox (account_id,to_address,subject,html,text,kind)
           VALUES ($1,$2,$3,$4,$5,$6) RETURNING mail_id"#,
    )
    .bind(account_id)
    .bind(to)
    .bind(&email.subject)
    .bind(&email.html)
    .bind(&email.text)
    .bind(kind)
    .fetch_one(db)
    .await?;
    // The notification service polls, so it does not need this to find the row.
    // It is published for everything else: a listener that wants to count what
    // was sent to whom should not have to poll the same table to learn.
    crate::bus::publish(
        crate::bus::subject::MAIL_QUEUED,
        account_id,
        serde_json::json!({ "mail_id": id, "to": to, "kind": kind, "subject": email.subject }),
    )
    .await;
    Ok(id)
}

/// One message owed, as the notification service sees it.
pub struct OutboundMail {
    pub mail_id: i64,
    pub to: String,
    pub attempts: i32,
    pub email: crate::mail::Email,
}

/// Claim the next message that is owed and due.
///
/// `FOR UPDATE SKIP LOCKED` and the `NextAttemptAt` bump in the same statement:
/// claiming *is* scheduling the retry, so a service that dies mid-send leaves
/// the row due again shortly rather than locked forever. Sending is therefore
/// at-least-once — a crash after the provider accepted but before `SentAt` is
/// written sends twice. For mail that is the right side to err on.
pub async fn claim_due_mail(db: &Db, retry_after_seconds: i64) -> Result<Option<OutboundMail>> {
    let row = sqlx::query(
        r#"UPDATE mail_outbox
           SET attempts = attempts + 1,
               next_attempt_at = now() + make_interval(secs => $1::double precision)
           WHERE mail_id = (
               SELECT mail_id FROM mail_outbox
               WHERE sent_at IS NULL AND next_attempt_at <= now()
               ORDER BY next_attempt_at
               LIMIT 1
               FOR UPDATE SKIP LOCKED
           )
           RETURNING mail_id,to_address,subject,html,text,attempts"#,
    )
    .bind(retry_after_seconds as f64)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| OutboundMail {
        mail_id: r.get::<i64, _>("mail_id"),
        to: r.get::<String, _>("to_address"),
        attempts: r.get::<i32, _>("attempts"),
        email: crate::mail::Email {
            subject: r.get::<String, _>("subject"),
            html: r.get::<String, _>("html"),
            text: r.get::<String, _>("text"),
        },
    }))
}

pub async fn mark_mail_sent(db: &Db, mail_id: i64) -> Result<()> {
    sqlx::query(r#"UPDATE mail_outbox SET sent_at=now(), last_error='' WHERE mail_id=$1"#)
        .bind(mail_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Record why a send failed, and when to try again. `retry_in` of `None` gives
/// up: `SentAt` stays NULL so the row still reads as never sent, but
/// `NextAttemptAt` is pushed far enough out that nothing picks it up again.
pub async fn mark_mail_failed(db: &Db, mail_id: i64, err: &str, retry_in: Option<i64>) -> Result<()> {
    let secs = retry_in.unwrap_or(86_400 * 365) as f64;
    sqlx::query(
        r#"UPDATE mail_outbox
           SET last_error=$2, next_attempt_at = now() + make_interval(secs => $3::double precision)
           WHERE mail_id=$1"#,
    )
    .bind(mail_id)
    .bind(err.chars().take(500).collect::<String>())
    .bind(secs)
    .execute(db)
    .await?;
    Ok(())
}

pub fn public_draft_status(status: &str) -> String {
    match normalize_draft_status(status).as_str() {
        "queued" => "drafting".to_string(),
        other => other.to_string(),
    }
}

/// Moves a plan through drafting. Scoped by account like every other write.
pub async fn set_draft_status(db: &Db, account_id: i64, plan_id: i64, status: &str) -> Result<()> {
    sqlx::query(r#"UPDATE plan SET draft_status=$3, updated_at=now() WHERE account_id=$1 AND plan_id=$2"#)
        .bind(account_id)
        .bind(plan_id)
        .bind(normalize_draft_status(status))
        .execute(db)
        .await?;
    Ok(())
}

/// Queue a plan for the planning service, parking the brief on the row.
///
/// `adopt_name` rides inside the envelope rather than in its own column: it is
/// part of "what was asked for", and one column that either holds the whole
/// request or nothing is easier to reason about than two that can disagree.
pub async fn queue_draft(
    db: &Db,
    account_id: i64,
    plan_id: i64,
    req: &crate::plan_chat::PlanDraftRequest,
    adopt_name: bool,
) -> Result<()> {
    let envelope = serde_json::to_string(&serde_json::json!({
        "req": req,
        "adopt_name": adopt_name,
    }))?;
    sqlx::query(
        r#"UPDATE plan SET draft_status='queued', draft_request_json=$3, updated_at=now()
           WHERE account_id=$1 AND plan_id=$2"#,
    )
    .bind(account_id)
    .bind(plan_id)
    .bind(envelope)
    .execute(db)
    .await?;
    Ok(())
}

/// Queue a redraft with no brief: the planning service rebuilds the request
/// from the plan itself. What `POST /drafts` and a rebuild both want.
pub async fn queue_draft_bare(db: &Db, account_id: i64, plan_id: i64) -> Result<()> {
    sqlx::query(
        r#"UPDATE plan SET draft_status='queued', draft_request_json='', updated_at=now()
           WHERE account_id=$1 AND plan_id=$2"#,
    )
    .bind(account_id)
    .bind(plan_id)
    .execute(db)
    .await?;
    Ok(())
}

/// How many plans are waiting to be drafted.
pub async fn queued_draft_count(db: &Db) -> Result<i64> {
    Ok(sqlx::query_scalar(r#"SELECT count(*) FROM plan WHERE draft_status='queued'"#).fetch_one(db).await?)
}

/// One claimed draft: which plan, whose, and the brief that was queued with it.
pub struct ClaimedDraft {
    pub plan_id: i64,
    pub account_id: i64,
    /// `None` when the row carried no request — a redraft, which the planning
    /// service rebuilds from the plan itself.
    pub request: Option<crate::plan_chat::PlanDraftRequest>,
    pub adopt_name: bool,
}

/// Claim one queued plan for drafting, atomically.
///
/// `FOR UPDATE SKIP LOCKED` on the inner select is what makes this safe: two
/// planning replicas racing pick different rows rather than blocking, and the
/// outer UPDATE's own `DraftStatus='queued'` is re-checked under the row lock,
/// so neither can claim a plan the other already took.
///
/// Not account-scoped, deliberately — this is a service claiming work across
/// every tenant, the same way the run pool does. The account comes back with
/// the row and everything downstream filters on it.
pub async fn claim_queued_draft(db: &Db) -> Result<Option<ClaimedDraft>> {
    let row = sqlx::query(
        r#"UPDATE plan SET draft_status='drafting', updated_at=now()
           WHERE plan_id = (
               SELECT plan_id FROM plan
               WHERE draft_status='queued'
               ORDER BY updated_at
               LIMIT 1
               FOR UPDATE SKIP LOCKED
           )
           AND draft_status='queued'
           RETURNING plan_id, account_id, draft_request_json"#,
    )
    .fetch_optional(db)
    .await?;
    let Some(r) = row else { return Ok(None) };
    let raw: String = r.get("draft_request_json");
    // A malformed envelope must not wedge the queue: fall back to rebuilding
    // the request from the plan, which is always possible.
    let parsed: Option<Value> = if raw.trim().is_empty() { None } else { serde_json::from_str(&raw).ok() };
    let adopt_name = parsed.as_ref().and_then(|v| v["adopt_name"].as_bool()).unwrap_or(false);
    let request = parsed
        .as_ref()
        .map(|v| v["req"].clone())
        .and_then(|v| serde_json::from_value::<crate::plan_chat::PlanDraftRequest>(v).ok());
    Ok(Some(ClaimedDraft {
        plan_id: r.get::<i64, _>("plan_id"),
        account_id: r.get::<i64, _>("account_id"),
        request,
        adopt_name,
    }))
}

/// Plans left 'drafting' by a planning service that died mid-draft. Requeued on
/// startup rather than failed: nothing was written, so a retry is free, and a
/// plan stuck in 'drafting' forever is invisible to the user as anything but a
/// spinner that never stops.
pub async fn requeue_abandoned_drafts(db: &Db, older_than_minutes: i64) -> Result<u64> {
    let n = sqlx::query(
        r#"UPDATE plan SET draft_status='queued'
           WHERE draft_status='drafting'
             AND updated_at < now() - make_interval(mins => $1::int)"#,
    )
    .bind(older_than_minutes as i32)
    .execute(db)
    .await?
    .rows_affected();
    Ok(n)
}

/// How hard a run tries, as one word. Anything unrecognised reads as 'normal',
/// so a plan drafted before this existed behaves the way it always did.
pub fn normalize_effort(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "quick" => "quick",
        "thorough" => "thorough",
        "exhaustive" => "exhaustive",
        _ => "normal",
    }
    .to_string()
}

/// The machine settings behind an effort level: (iterations, zero-yield limit,
/// learn mode). Learn is off for a quick look — a single pass has nothing to
/// learn between.
pub fn effort_settings(effort: &str) -> (i32, i32, bool) {
    match normalize_effort(effort).as_str() {
        "quick" => (1, 1, false),
        "thorough" => (6, 3, true),
        "exhaustive" => (12, 4, true),
        _ => (3, 2, true),
    }
}

/// Inserts (plan_id == 0) or updates a plan. Returns the plan id.
pub async fn save_plan(db: &Db, account_id: i64, sc: &SourceConfig) -> Result<i64> {
    let plan_type = normalize_plan_type(&sc.plan_type);
    let q = if sc.plan_id == 0 {
        r#"INSERT INTO plan (account_id,source,description,plan_type,scrape_prompt,enrich_prompt,planner_prompt,source_key_tmpl,name_tmpl,title_tmpl,company_tmpl,industry_tmpl,email_tmpl,email_status_tmpl,phone_tmpl,website_tmpl,linkedin_tmpl,location_tmpl,notes_tmpl,estimated_value_tmpl,min_value,learn,iterations,max_no_progress,known_limit,target_prospects,free_agent,favorite,model,seed_vars_json,allow_hosts,sites,alert_email,schedule_enabled,schedule_time,schedule_days,kind,fields_schema_json,subject,effort,draft_status)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25,$26,$27,$28,$29,$30,$31,$32,$33,$34,$35,$36,$37,$38,$39,$40,$41)
           RETURNING plan_id"#
    } else {
        r#"UPDATE plan SET source=$2,description=$3,plan_type=$4,scrape_prompt=$5,enrich_prompt=$6,planner_prompt=$7,source_key_tmpl=$8,name_tmpl=$9,title_tmpl=$10,company_tmpl=$11,industry_tmpl=$12,email_tmpl=$13,email_status_tmpl=$14,phone_tmpl=$15,website_tmpl=$16,linkedin_tmpl=$17,location_tmpl=$18,notes_tmpl=$19,estimated_value_tmpl=$20,min_value=$21,learn=$22,iterations=$23,max_no_progress=$24,known_limit=$25,target_prospects=$26,free_agent=$27,favorite=$28,model=$29,seed_vars_json=$30,allow_hosts=$31,sites=$32,alert_email=$33,schedule_enabled=$34,schedule_time=$35,schedule_days=$36,kind=$37,fields_schema_json=$38,subject=$39,effort=$40,draft_status=$41,updated_at=now()
           WHERE account_id=$1 AND plan_id=$42
           RETURNING plan_id"#
    };
    let mut query = sqlx::query_scalar::<_, i64>(q)
        .bind(account_id)
        .bind(sc.source.trim())
        .bind(sc.description.trim())
        .bind(&plan_type)
        .bind(&sc.scrape_prompt)
        .bind(&sc.enrich_prompt)
        .bind(&sc.planner_prompt)
        .bind(&sc.source_key_tmpl)
        .bind(&sc.name_tmpl)
        .bind(&sc.title_tmpl)
        .bind(&sc.company_tmpl)
        .bind(&sc.industry_tmpl)
        .bind(&sc.email_tmpl)
        .bind(&sc.email_status_tmpl)
        .bind(&sc.phone_tmpl)
        .bind(&sc.website_tmpl)
        .bind(&sc.linkedin_tmpl)
        .bind(&sc.location_tmpl)
        .bind(&sc.notes_tmpl)
        .bind(&sc.estimated_value_tmpl)
        .bind(sc.min_value)
        .bind(sc.learn)
        .bind(sc.iterations)
        .bind(sc.max_no_progress)
        .bind(sc.known_limit)
        .bind(sc.target_prospects)
        .bind(sc.free_agent)
        .bind(sc.favorite)
        .bind(&sc.model)
        .bind(&sc.seed_vars_json)
        .bind(&sc.allow_hosts)
        .bind(&sc.sites)
        .bind(sc.alert_email)
        .bind(sc.schedule_enabled)
        .bind(&sc.schedule_time)
        .bind(&sc.schedule_days)
        .bind(sc.kind_of().as_str())
        .bind(&sc.fields_schema_json)
        .bind(sc.subject.trim())
        .bind(normalize_effort(&sc.effort))
        .bind(normalize_draft_status(&sc.draft_status));
    if sc.plan_id != 0 {
        query = query.bind(sc.plan_id);
    }
    let id = query.fetch_optional(db).await.map_err(|e| match e {
        sqlx::Error::Database(ref d) if d.is_unique_violation() => anyhow!("you already have a plan named {:?}", sc.source.trim()),
        e => anyhow!(e),
    })?;
    let id = id.ok_or_else(|| anyhow!("plan not found"))?;
    sqlx::query(
        r#"UPDATE plan SET model_scrape=$3, model_enrich=$4, model_planner=$5
           WHERE account_id=$1 AND plan_id=$2"#,
    )
    .bind(account_id)
    .bind(id)
    .bind(sc.model_scrape.trim())
    .bind(sc.model_enrich.trim())
    .bind(sc.model_planner.trim())
    .execute(db)
    .await?;
    Ok(id)
}

pub async fn delete_plan(db: &Db, account_id: i64, plan_id: i64) -> Result<bool> {
    let n = sqlx::query(r#"DELETE FROM plan WHERE account_id = $1 AND plan_id = $2"#)
        .bind(account_id)
        .bind(plan_id)
        .execute(db)
        .await?
        .rows_affected();
    Ok(n > 0)
}

pub async fn set_favorite(db: &Db, account_id: i64, plan_id: i64, fav: bool) -> Result<()> {
    sqlx::query(r#"UPDATE plan SET favorite = $3 WHERE account_id = $1 AND plan_id = $2"#)
        .bind(account_id)
        .bind(plan_id)
        .bind(fav)
        .execute(db)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Prospects
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ProspectRow {
    pub prospect_id: i64,
    pub plan_id: i64,
    pub name: String,
    pub title: String,
    pub company: String,
    pub industry: String,
    pub email: String,
    pub email_status: String,
    pub phone: String,
    pub website: String,
    pub linkedin: String,
    pub location: String,
    pub notes: String,
    pub estimated_value: Option<i64>,
    pub source: String,
    pub source_key: String,
    pub first_seen_utc: DateTime<Utc>,
    pub last_seen_utc: DateTime<Utc>,
}

const PROSPECT_COLS: &str = r#"pr.prospect_id, pr.plan_id, pr.name, pr.title, pr.company, pr.industry, pr.email,
pr.email_status, pr.phone, pr.website, pr.linkedin, pr.location, pr.notes,
pr.estimated_value, p.source, pr.source_key, pr.first_seen_utc, pr.last_seen_utc"#;

pub async fn upsert_prospect(db: &Db, account_id: i64, plan_id: i64, p: &Prospect) -> Result<()> {
    if p.name.trim().is_empty() {
        bail!("refusing to store a prospect with no name (key {})", p.source_key);
    }
    let meta: Value = serde_json::from_str(&p.metadata).unwrap_or(Value::Object(Default::default()));
    sqlx::query(
        r#"INSERT INTO prospect (plan_id,account_id,name,title,company,industry,email,email_status,phone,
             website,linkedin,location,notes,estimated_value,meta_data,source_key)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)
           ON CONFLICT (plan_id,source_key) DO UPDATE SET
             name=excluded.name, title=excluded.title, company=excluded.company, industry=excluded.industry,
             email=excluded.email, email_status=excluded.email_status, phone=excluded.phone, website=excluded.website,
             linkedin=excluded.linkedin, location=excluded.location, notes=excluded.notes,
             estimated_value=excluded.estimated_value, meta_data=excluded.meta_data,
             last_seen_utc=now()"#,
    )
    .bind(plan_id)
    .bind(account_id)
    .bind(&p.name)
    .bind(&p.title)
    .bind(&p.company)
    .bind(&p.industry)
    .bind(&p.email)
    .bind(&p.email_status)
    .bind(&p.phone)
    .bind(&p.website)
    .bind(&p.linkedin)
    .bind(&p.location)
    .bind(&p.notes)
    .bind(p.estimated_value)
    .bind(meta)
    .bind(&p.source_key)
    .execute(db)
    .await?;
    Ok(())
}

/// How many rows a plan holds now, whatever kind it is.
pub async fn plan_row_count(db: &Db, plan_id: i64, kind: &str) -> i64 {
    let sql = match kind {
        "artifacts" => r#"SELECT count(*) FROM artifact WHERE plan_id=$1"#,
        "assets" => r#"SELECT count(*) FROM asset WHERE plan_id=$1"#,
        "report" => r#"SELECT count(*) FROM report WHERE plan_id=$1"#,
        _ => r#"SELECT count(*) FROM prospect WHERE plan_id=$1"#,
    };
    sqlx::query_scalar::<_, i64>(sql).bind(plan_id).fetch_one(db).await.unwrap_or(0)
}

/// A few human labels for the newest rows a plan holds, for the alert email.
///
/// One query per kind because the tables are genuinely different, and a label
/// is whatever a person would recognise the row by. Failure is not worth
/// propagating — an alert with no samples is still a useful alert.
pub async fn recent_labels(db: &Db, plan_id: i64, kind: &str, limit: i64) -> Vec<String> {
    let sql = match kind {
        "artifacts" => r#"SELECT title FROM artifact WHERE plan_id=$1 ORDER BY first_seen_utc DESC LIMIT $2"#,
        "assets" => r#"SELECT title FROM asset WHERE plan_id=$1 ORDER BY first_seen_utc DESC LIMIT $2"#,
        "report" => return Vec::new(),
        _ => r#"SELECT name FROM prospect WHERE plan_id=$1 ORDER BY first_seen_utc DESC LIMIT $2"#,
    };
    sqlx::query_scalar::<_, String>(sql)
        .bind(plan_id)
        .bind(limit)
        .fetch_all(db)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .collect()
}

pub async fn known_source_keys(db: &Db, plan_id: i64) -> Result<HashSet<String>> {
    let keys: Vec<String> = sqlx::query_scalar(r#"SELECT source_key FROM prospect WHERE plan_id = $1"#)
        .bind(plan_id)
        .fetch_all(db)
        .await?;
    Ok(keys.into_iter().collect())
}

/// Companies (or people, on an individual plan) stored most recently, for the
/// exclusion list replayed into prompts.
pub async fn recent_entities(db: &Db, plan_id: i64, limit: i32, plan_type: &str) -> Result<Vec<String>> {
    // Spliced in as an identifier (a bind parameter cannot name a column), so it
    // has to be the column's real, lowercase name.
    let col = if is_individual(plan_type) { "name" } else { "company" };
    let rows: Vec<String> = sqlx::query_scalar(&format!(
        r#"SELECT DISTINCT ON (lower({col})) {col} FROM prospect
           WHERE plan_id = $1 AND {col} <> ''
           ORDER BY lower({col}), last_seen_utc DESC LIMIT $2"#
    ))
    .bind(plan_id)
    .bind(limit.max(0) as i64)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

pub struct ProspectFilter {
    pub plan_id: Option<i64>,
    pub min_value: i64,
    pub search: String,
    pub limit: i64,
    pub offset: i64,
}

pub async fn list_prospects(db: &Db, account_id: i64, f: &ProspectFilter) -> Result<(Vec<ProspectRow>, i64)> {
    let pattern = format!("%{}%", f.search.trim().to_lowercase());
    let where_clause = r#"pr.account_id = $1
          AND ($2::bigint IS NULL OR pr.plan_id = $2)
          AND ($3::bigint <= 0 OR pr.estimated_value >= $3)
          AND ($4 = '%%' OR lower(pr.name || ' ' || pr.company || ' ' || pr.title || ' ' || pr.email || ' ' || pr.location || ' ' || pr.website) LIKE $4)"#;
    let rows = sqlx::query(&format!(
        r#"SELECT {PROSPECT_COLS} FROM prospect pr JOIN plan p ON p.plan_id = pr.plan_id
           WHERE {where_clause}
           ORDER BY pr.last_seen_utc DESC, pr.prospect_id DESC LIMIT $5 OFFSET $6"#
    ))
    .bind(account_id)
    .bind(f.plan_id)
    .bind(f.min_value)
    .bind(&pattern)
    .bind(f.limit.clamp(1, 5000))
    .bind(f.offset.max(0))
    .fetch_all(db)
    .await?;
    let total: i64 = sqlx::query_scalar(&format!(
        r#"SELECT count(*) FROM prospect pr WHERE {where_clause}"#
    ))
    .bind(account_id)
    .bind(f.plan_id)
    .bind(f.min_value)
    .bind(&pattern)
    .fetch_one(db)
    .await?;
    let out = rows.iter().map(ProspectRow::from_row).collect::<Result<Vec<_>, _>>()?;
    Ok((out, total))
}

/// Every matching row, for exports.
pub async fn export_prospects(db: &Db, account_id: i64, plan_id: Option<i64>, min_value: i64) -> Result<Vec<ProspectRow>> {
    let rows = sqlx::query(&format!(
        r#"SELECT {PROSPECT_COLS} FROM prospect pr JOIN plan p ON p.plan_id = pr.plan_id
           WHERE pr.account_id = $1 AND ($2::bigint IS NULL OR pr.plan_id = $2) AND ($3::bigint <= 0 OR pr.estimated_value >= $3)
           ORDER BY p.source, pr.prospect_id"#
    ))
    .bind(account_id)
    .bind(plan_id)
    .bind(min_value)
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(ProspectRow::from_row).collect::<Result<Vec<_>, _>>()?)
}

pub async fn delete_prospects(db: &Db, account_id: i64, plan_id: Option<i64>) -> Result<u64> {
    Ok(sqlx::query(r#"DELETE FROM prospect WHERE account_id = $1 AND ($2::bigint IS NULL OR plan_id = $2)"#)
        .bind(account_id)
        .bind(plan_id)
        .execute(db)
        .await?
        .rows_affected())
}

pub async fn delete_prospect(db: &Db, account_id: i64, prospect_id: i64) -> Result<bool> {
    Ok(sqlx::query(r#"DELETE FROM prospect WHERE account_id = $1 AND prospect_id = $2"#)
        .bind(account_id)
        .bind(prospect_id)
        .execute(db)
        .await?
        .rows_affected()
        > 0)
}

/// A monotonically increasing version for an account's prospects, for the
/// download API's ETag. Derived from the newest write rather than a counter,
/// so it needs no bump on every insert.
pub async fn prospect_version(db: &Db, account_id: i64, plan_id: Option<i64>) -> Result<(i64, i64)> {
    let row = sqlx::query(
        r#"SELECT count(*) AS n, coalesce(extract(epoch FROM max(last_seen_utc))::bigint, 0) AS v
           FROM prospect WHERE account_id = $1 AND ($2::bigint IS NULL OR plan_id = $2)"#,
    )
    .bind(account_id)
    .bind(plan_id)
    .fetch_one(db)
    .await?;
    Ok((row.try_get("n")?, row.try_get("v")?))
}

// ---------------------------------------------------------------------------
// Artifacts (custom-schema rows) — the parallel of prospects for Kind='artifacts'
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ArtifactRow {
    pub artifact_id: i64,
    pub plan_id: i64,
    pub source_key: String,
    pub title: String,
    pub url: String,
    #[sqlx(rename = "fields_json")]
    pub fields: Value,
    pub source: String,
    pub first_seen_utc: DateTime<Utc>,
    pub last_seen_utc: DateTime<Utc>,
}

const ARTIFACT_COLS: &str = r#"a.artifact_id, a.plan_id, a.source_key, a.title, a.url, a.fields_json,
p.source, a.first_seen_utc, a.last_seen_utc"#;

pub async fn upsert_artifact(db: &Db, account_id: i64, plan_id: i64, a: &crate::artifact::Artifact) -> Result<()> {
    let fields: Value = serde_json::from_str(&a.fields_json).unwrap_or(Value::Object(Default::default()));
    let meta: Value = serde_json::from_str(&a.metadata).unwrap_or(Value::Object(Default::default()));
    sqlx::query(
        r#"INSERT INTO artifact (plan_id,account_id,source_key,title,url,fields_json,meta_data)
           VALUES ($1,$2,$3,$4,$5,$6,$7)
           ON CONFLICT (plan_id,source_key) DO UPDATE SET
             title=excluded.title, url=excluded.url, fields_json=excluded.fields_json,
             meta_data=excluded.meta_data, last_seen_utc=now()"#,
    )
    .bind(plan_id)
    .bind(account_id)
    .bind(&a.source_key)
    .bind(&a.title)
    .bind(&a.url)
    .bind(fields)
    .bind(meta)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn known_artifact_keys(db: &Db, plan_id: i64) -> Result<HashSet<String>> {
    let keys: Vec<String> = sqlx::query_scalar(r#"SELECT source_key FROM artifact WHERE plan_id = $1"#)
        .bind(plan_id)
        .fetch_all(db)
        .await?;
    Ok(keys.into_iter().collect())
}

pub struct ArtifactFilter {
    pub plan_id: Option<i64>,
    pub search: String,
    pub limit: i64,
    pub offset: i64,
}

pub async fn list_artifacts(db: &Db, account_id: i64, f: &ArtifactFilter) -> Result<(Vec<ArtifactRow>, i64)> {
    let pattern = format!("%{}%", f.search.trim().to_lowercase());
    let where_clause = r#"a.account_id = $1
          AND ($2::bigint IS NULL OR a.plan_id = $2)
          AND ($3 = '%%' OR lower(a.title || ' ' || a.fields_json::text) LIKE $3)"#;
    let rows = sqlx::query(&format!(
        r#"SELECT {ARTIFACT_COLS} FROM artifact a JOIN plan p ON p.plan_id = a.plan_id
           WHERE {where_clause}
           ORDER BY a.last_seen_utc DESC, a.artifact_id DESC LIMIT $4 OFFSET $5"#
    ))
    .bind(account_id)
    .bind(f.plan_id)
    .bind(&pattern)
    .bind(f.limit.clamp(1, 5000))
    .bind(f.offset.max(0))
    .fetch_all(db)
    .await?;
    let total: i64 = sqlx::query_scalar(&format!(
        r#"SELECT count(*) FROM artifact a WHERE {where_clause}"#
    ))
    .bind(account_id)
    .bind(f.plan_id)
    .bind(&pattern)
    .fetch_one(db)
    .await?;
    let out = rows.iter().map(ArtifactRow::from_row).collect::<Result<Vec<_>, _>>()?;
    Ok((out, total))
}

pub async fn export_artifacts(db: &Db, account_id: i64, plan_id: Option<i64>) -> Result<Vec<ArtifactRow>> {
    let rows = sqlx::query(&format!(
        r#"SELECT {ARTIFACT_COLS} FROM artifact a JOIN plan p ON p.plan_id = a.plan_id
           WHERE a.account_id = $1 AND ($2::bigint IS NULL OR a.plan_id = $2)
           ORDER BY a.artifact_id"#
    ))
    .bind(account_id)
    .bind(plan_id)
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(ArtifactRow::from_row).collect::<Result<Vec<_>, _>>()?)
}

pub async fn delete_artifacts(db: &Db, account_id: i64, plan_id: Option<i64>) -> Result<u64> {
    Ok(sqlx::query(r#"DELETE FROM artifact WHERE account_id = $1 AND ($2::bigint IS NULL OR plan_id = $2)"#)
        .bind(account_id)
        .bind(plan_id)
        .execute(db)
        .await?
        .rows_affected())
}

pub async fn delete_artifact(db: &Db, account_id: i64, artifact_id: i64) -> Result<bool> {
    Ok(sqlx::query(r#"DELETE FROM artifact WHERE account_id = $1 AND artifact_id = $2"#)
        .bind(account_id)
        .bind(artifact_id)
        .execute(db)
        .await?
        .rows_affected()
        > 0)
}

// ---------------------------------------------------------------------------
// Reports (one document per subject) — Kind='report'
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ReportRow {
    pub report_id: i64,
    pub plan_id: i64,
    pub execution_id: Option<i64>,
    pub source_key: String,
    pub subject: String,
    pub title: String,
    pub markdown: String,
    #[sqlx(rename = "sources_json")]
    pub sources: Value,
    pub word_count: i32,
    pub source: String,
    pub first_seen_utc: DateTime<Utc>,
    pub last_seen_utc: DateTime<Utc>,
}

const REPORT_COLS: &str = r#"r.report_id, r.plan_id, r.execution_id, r.source_key, r.subject, r.title,
r.markdown, r.sources_json, r.word_count, p.source, r.first_seen_utc, r.last_seen_utc"#;

/// One report per (plan, subject): re-running refreshes the document rather
/// than piling up versions.
pub struct NewReport {
    pub source_key: String,
    pub subject: String,
    pub title: String,
    pub markdown: String,
    pub sources: Value,
}

pub async fn upsert_report(db: &Db, account_id: i64, plan_id: i64, execution_id: Option<i64>, r: &NewReport) -> Result<()> {
    let words = r.markdown.split_whitespace().count() as i32;
    sqlx::query(
        r#"INSERT INTO report (plan_id,account_id,execution_id,source_key,subject,title,markdown,sources_json,word_count)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
           ON CONFLICT (plan_id,source_key) DO UPDATE SET
             execution_id=excluded.execution_id, subject=excluded.subject, title=excluded.title,
             markdown=excluded.markdown, sources_json=excluded.sources_json,
             word_count=excluded.word_count, last_seen_utc=now()"#,
    )
    .bind(plan_id)
    .bind(account_id)
    .bind(execution_id)
    .bind(&r.source_key)
    .bind(&r.subject)
    .bind(&r.title)
    .bind(&r.markdown)
    .bind(&r.sources)
    .bind(words)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn list_reports(db: &Db, account_id: i64, plan_id: Option<i64>) -> Result<Vec<ReportRow>> {
    let rows = sqlx::query(&format!(
        r#"SELECT {REPORT_COLS} FROM report r JOIN plan p ON p.plan_id = r.plan_id
           WHERE r.account_id = $1 AND ($2::bigint IS NULL OR r.plan_id = $2)
           ORDER BY r.last_seen_utc DESC, r.report_id DESC"#
    ))
    .bind(account_id)
    .bind(plan_id)
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(ReportRow::from_row).collect::<Result<Vec<_>, _>>()?)
}

pub async fn get_report(db: &Db, account_id: i64, report_id: i64) -> Result<Option<ReportRow>> {
    let row = sqlx::query(&format!(
        r#"SELECT {REPORT_COLS} FROM report r JOIN plan p ON p.plan_id = r.plan_id
           WHERE r.account_id = $1 AND r.report_id = $2"#
    ))
    .bind(account_id)
    .bind(report_id)
    .fetch_optional(db)
    .await?;
    row.map(|r| ReportRow::from_row(&r).map_err(Into::into)).transpose()
}

pub async fn delete_report(db: &Db, account_id: i64, report_id: i64) -> Result<bool> {
    Ok(sqlx::query(r#"DELETE FROM report WHERE account_id = $1 AND report_id = $2"#)
        .bind(account_id)
        .bind(report_id)
        .execute(db)
        .await?
        .rows_affected()
        > 0)
}

// ---------------------------------------------------------------------------
// Assets (collected files) — Kind='assets'. Bytes live in objstore.rs.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct AssetRow {
    pub asset_id: i64,
    pub plan_id: i64,
    pub execution_id: Option<i64>,
    pub source_key: String,
    pub title: String,
    pub source_url: String,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    #[serde(skip_serializing)]
    pub object_key: String,
    pub source: String,
    pub first_seen_utc: DateTime<Utc>,
    pub last_seen_utc: DateTime<Utc>,
}

const ASSET_COLS: &str = r#"a.asset_id, a.plan_id, a.execution_id, a.source_key, a.title, a.source_url,
a.filename, a.content_type, a.size_bytes, a.object_key, p.source, a.first_seen_utc, a.last_seen_utc"#;

pub struct NewAsset {
    /// sha256 of the bytes — the dedupe identity.
    pub source_key: String,
    pub title: String,
    pub source_url: String,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub object_key: String,
    pub metadata: Value,
}

pub async fn upsert_asset(db: &Db, account_id: i64, plan_id: i64, execution_id: Option<i64>, a: &NewAsset) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO asset (plan_id,account_id,execution_id,source_key,sha256,source_url,title,
             filename,content_type,size_bytes,object_key,meta_data)
           VALUES ($1,$2,$3,$4,$4,$5,$6,$7,$8,$9,$10,$11)
           ON CONFLICT (plan_id,source_key) DO UPDATE SET
             execution_id=excluded.execution_id, title=excluded.title, source_url=excluded.source_url,
             filename=excluded.filename, content_type=excluded.content_type,
             size_bytes=excluded.size_bytes, object_key=excluded.object_key,
             meta_data=excluded.meta_data, last_seen_utc=now()"#,
    )
    .bind(plan_id)
    .bind(account_id)
    .bind(execution_id)
    .bind(&a.source_key)
    .bind(&a.source_url)
    .bind(&a.title)
    .bind(&a.filename)
    .bind(&a.content_type)
    .bind(a.size_bytes)
    .bind(&a.object_key)
    .bind(&a.metadata)
    .execute(db)
    .await?;
    Ok(())
}

/// Both the content hashes and the URLs already collected, so a run can skip a
/// download before spending the bandwidth as well as after.
pub async fn known_asset_keys(db: &Db, plan_id: i64) -> Result<(HashSet<String>, HashSet<String>)> {
    let rows: Vec<(String, String)> =
        sqlx::query_as(r#"SELECT source_key, source_url FROM asset WHERE plan_id = $1"#)
            .bind(plan_id)
            .fetch_all(db)
            .await?;
    let mut keys = HashSet::new();
    let mut urls = HashSet::new();
    for (k, u) in rows {
        keys.insert(k);
        if !u.is_empty() {
            urls.insert(u);
        }
    }
    Ok((keys, urls))
}

pub async fn list_assets(db: &Db, account_id: i64, plan_id: Option<i64>) -> Result<Vec<AssetRow>> {
    let rows = sqlx::query(&format!(
        r#"SELECT {ASSET_COLS} FROM asset a JOIN plan p ON p.plan_id = a.plan_id
           WHERE a.account_id = $1 AND ($2::bigint IS NULL OR a.plan_id = $2)
           ORDER BY a.last_seen_utc DESC, a.asset_id DESC"#
    ))
    .bind(account_id)
    .bind(plan_id)
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(AssetRow::from_row).collect::<Result<Vec<_>, _>>()?)
}

pub async fn get_asset(db: &Db, account_id: i64, asset_id: i64) -> Result<Option<AssetRow>> {
    let row = sqlx::query(&format!(
        r#"SELECT {ASSET_COLS} FROM asset a JOIN plan p ON p.plan_id = a.plan_id
           WHERE a.account_id = $1 AND a.asset_id = $2"#
    ))
    .bind(account_id)
    .bind(asset_id)
    .fetch_optional(db)
    .await?;
    row.map(|r| AssetRow::from_row(&r).map_err(Into::into)).transpose()
}

/// Returns the object key so the caller can drop the bytes too.
pub async fn delete_asset(db: &Db, account_id: i64, asset_id: i64) -> Result<Option<String>> {
    Ok(sqlx::query_scalar(
        r#"DELETE FROM asset WHERE account_id = $1 AND asset_id = $2 RETURNING object_key"#,
    )
    .bind(account_id)
    .bind(asset_id)
    .fetch_optional(db)
    .await?)
}

pub struct ResultsFilter {
    pub plan_id: Option<i64>,
    pub search: String,
    pub limit: i64,
    pub offset: i64,
}

/// A unified, searchable view across every plan's results — prospects, custom
/// artifacts, reports and collected files together — projected onto common
/// columns (label, plan, kind, url, seen). Lets the Results page search
/// anything from any plan even though the per-kind tables differ completely.
pub async fn list_results(db: &Db, account_id: i64, f: &ResultsFilter) -> Result<(Vec<Value>, i64)> {
    let pattern = format!("%{}%", f.search.trim().to_lowercase());
    let cte = r#"WITH r AS (
        SELECT 'prospect' AS kind, pr.plan_id AS plan_id, p.source AS plan,
               coalesce(nullif(pr.name,''), nullif(pr.company,''), pr.source_key) AS label,
               nullif(pr.company,'') AS sublabel, pr.website AS url, pr.last_seen_utc AS seen,
               lower(pr.name||' '||pr.company||' '||pr.title||' '||pr.email||' '||pr.location||' '||pr.website) AS hay
        FROM prospect pr JOIN plan p ON p.plan_id=pr.plan_id WHERE pr.account_id=$1
        UNION ALL
        SELECT 'artifact', a.plan_id, p.source,
               coalesce(nullif(a.title,''), a.source_key), NULL, a.url, a.last_seen_utc,
               lower(a.title||' '||a.fields_json::text)
        FROM artifact a JOIN plan p ON p.plan_id=a.plan_id WHERE a.account_id=$1
        UNION ALL
        -- Reports search their whole body, so the Results page finds a plan by
        -- something mentioned inside the document.
        SELECT 'report', rp.plan_id, p.source,
               coalesce(nullif(rp.title,''), rp.subject), nullif(rp.subject,''), '', rp.last_seen_utc,
               lower(rp.title||' '||rp.subject||' '||rp.markdown)
        FROM report rp JOIN plan p ON p.plan_id=rp.plan_id WHERE rp.account_id=$1
        UNION ALL
        SELECT 'file', f.plan_id, p.source,
               coalesce(nullif(f.title,''), f.filename), nullif(f.filename,''), f.source_url, f.last_seen_utc,
               lower(f.title||' '||f.filename||' '||f.source_url)
        FROM asset f JOIN plan p ON p.plan_id=f.plan_id WHERE f.account_id=$1
    )"#;
    let filter = r#"WHERE ($2::bigint IS NULL OR plan_id = $2) AND ($3 = '%%' OR hay LIKE $3)"#;
    let rows = sqlx::query(&format!(
        "{cte} SELECT kind, plan_id, plan, label, sublabel, url, seen FROM r {filter} ORDER BY seen DESC LIMIT $4 OFFSET $5"
    ))
    .bind(account_id)
    .bind(f.plan_id)
    .bind(&pattern)
    .bind(f.limit.clamp(1, 500))
    .bind(f.offset.max(0))
    .fetch_all(db)
    .await?;
    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "kind": r.get::<String, _>("kind"),
                "plan_id": r.get::<i64, _>("plan_id"),
                "plan": r.get::<String, _>("plan"),
                "label": r.get::<Option<String>, _>("label").unwrap_or_default(),
                "sublabel": r.get::<Option<String>, _>("sublabel").unwrap_or_default(),
                "url": r.get::<String, _>("url"),
                "last_seen_utc": r.get::<DateTime<Utc>, _>("seen").to_rfc3339(),
            })
        })
        .collect();
    let total: i64 = sqlx::query_scalar(&format!("{cte} SELECT count(*) FROM r {filter}"))
        .bind(account_id)
        .bind(f.plan_id)
        .bind(&pattern)
        .fetch_one(db)
        .await?;
    Ok((out, total))
}

/// Dashboard numbers.
#[derive(Debug, Serialize)]
pub struct Overview {
    pub plans: i64,
    pub prospects: i64,
    pub prospects_7d: i64,
    pub executions: i64,
    pub active_executions: i64,
    pub last_execution_at: String,
}

pub async fn overview(db: &Db, account_id: i64) -> Result<Overview> {
    let row = sqlx::query(
        r#"SELECT
            (SELECT count(*) FROM plan WHERE account_id = $1) AS plans,
            (SELECT count(*) FROM prospect WHERE account_id = $1) AS prospects,
            (SELECT count(*) FROM prospect WHERE account_id = $1 AND first_seen_utc > now() - interval '7 days') AS prospects_7d,
            (SELECT count(*) FROM execution WHERE account_id = $1) AS executions,
            (SELECT count(*) FROM execution WHERE account_id = $1 AND status IN ('queued','running')) AS active_executions,
            (SELECT max(started_at) FROM execution WHERE account_id = $1) AS last_execution_at"#,
    )
    .bind(account_id)
    .fetch_one(db)
    .await?;
    Ok(Overview {
        plans: row.try_get("plans")?,
        prospects: row.try_get("prospects")?,
        prospects_7d: row.try_get("prospects_7d")?,
        executions: row.try_get("executions")?,
        active_executions: row.try_get("active_executions")?,
        last_execution_at: ts(row.try_get("last_execution_at")?),
    })
}

// ---------------------------------------------------------------------------
// Search frontier
// ---------------------------------------------------------------------------

pub async fn was_explored(db: &Db, plan_id: i64, seed_key: &str, ttl_hours: i64) -> Result<bool> {
    let n: i64 = if ttl_hours <= 0 {
        sqlx::query_scalar(r#"SELECT count(*) FROM search_frontier WHERE plan_id=$1 AND seed_key=$2 AND status='explored'"#)
            .bind(plan_id)
            .bind(seed_key)
            .fetch_one(db)
            .await?
    } else {
        sqlx::query_scalar(
            r#"SELECT count(*) FROM search_frontier WHERE plan_id=$1 AND seed_key=$2 AND status='explored'
               AND explored_at > now() - make_interval(hours => $3)"#,
        )
        .bind(plan_id)
        .bind(seed_key)
        .bind(ttl_hours as i32)
        .fetch_one(db)
        .await?
    };
    Ok(n > 0)
}

pub async fn mark_explored(db: &Db, plan_id: i64, seed_key: &str, seed_json: &str, iteration: i64, new_rows: i64) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO search_frontier (plan_id,seed_key,seed_json,iteration,new_prospects,status,explored_at)
           VALUES ($1,$2,$3,$4,$5,'explored',now())
           ON CONFLICT (plan_id,seed_key) DO UPDATE SET seed_json=excluded.seed_json, iteration=excluded.iteration,
             new_prospects=excluded.new_prospects, status='explored', explored_at=now()"#,
    )
    .bind(plan_id)
    .bind(seed_key)
    .bind(seed_json)
    .bind(iteration as i32)
    .bind(new_rows as i32)
    .execute(db)
    .await?;
    Ok(())
}

/// Queues a planner proposal. An explored seed is never demoted; a pending one
/// keeps its place. Returns whether a new row was written.
pub async fn queue_seed(db: &Db, plan_id: i64, seed_key: &str, seed_json: &str) -> Result<bool> {
    let n = sqlx::query(
        r#"INSERT INTO search_frontier (plan_id,seed_key,seed_json,status,queued_at,explored_at)
           VALUES ($1,$2,$3,'pending',now(),NULL) ON CONFLICT DO NOTHING"#,
    )
    .bind(plan_id)
    .bind(seed_key)
    .bind(seed_json)
    .execute(db)
    .await?
    .rows_affected();
    Ok(n > 0)
}

pub async fn pending_seeds(db: &Db, plan_id: i64, limit: i64) -> Result<Vec<(String, String)>> {
    let rows = sqlx::query(
        r#"SELECT seed_key,seed_json FROM search_frontier WHERE plan_id=$1 AND status='pending'
           ORDER BY queued_at ASC NULLS LAST LIMIT $2"#,
    )
    .bind(plan_id)
    .bind(limit.max(1))
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(|r| (r.get(0), r.get(1))).collect())
}

pub async fn pending_seed_count(db: &Db, plan_id: i64) -> Result<i64> {
    Ok(sqlx::query_scalar(r#"SELECT count(*) FROM search_frontier WHERE plan_id=$1 AND status='pending'"#)
        .bind(plan_id)
        .fetch_one(db)
        .await?)
}

pub async fn clear_pending_seeds(db: &Db, plan_id: i64) -> Result<u64> {
    Ok(sqlx::query(r#"DELETE FROM search_frontier WHERE plan_id=$1 AND status='pending'"#)
        .bind(plan_id)
        .execute(db)
        .await?
        .rows_affected())
}

pub async fn explored_seeds(db: &Db, plan_id: i64, limit: i64) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        r#"SELECT seed_json FROM search_frontier WHERE plan_id=$1 AND status='explored'
           ORDER BY explored_at DESC NULLS LAST LIMIT $2"#,
    )
    .bind(plan_id)
    .bind(limit)
    .fetch_all(db)
    .await?)
}

#[derive(Debug, Clone, Serialize)]
pub struct FrontierRow {
    pub seed_key: String,
    pub seed_json: String,
    pub status: String,
    pub iteration: Option<i32>,
    pub new_prospects: Option<i32>,
    pub queued_at: String,
    pub explored_at: String,
}

pub async fn list_frontier(db: &Db, plan_id: i64, limit: i64) -> Result<Vec<FrontierRow>> {
    let rows = sqlx::query(
        r#"SELECT seed_key,seed_json,status,iteration,new_prospects,queued_at,explored_at FROM search_frontier
           WHERE plan_id=$1 ORDER BY status DESC, coalesce(explored_at,queued_at) DESC LIMIT $2"#,
    )
    .bind(plan_id)
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| FrontierRow {
            seed_key: r.get(0),
            seed_json: r.get(1),
            status: r.get(2),
            iteration: r.get(3),
            new_prospects: r.get(4),
            queued_at: ts(r.get(5)),
            explored_at: ts(r.get(6)),
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Search trail
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SearchQueryRow {
    pub query_key: String,
    pub query: String,
    pub engine: String,
    pub hits: i64,
    pub max_depth: i64,
    pub new_prospects: i64,
    /// Approximate spend on this angle — see attribute_query_yield.
    #[serde(default)]
    pub tokens: i64,
    pub first_used_at: String,
    pub last_used_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VisitedPageRow {
    /// The page's own key — the same value trail edges use as `to`.
    pub url_key: String,
    pub url: String,
    pub host: String,
    pub visits: i64,
    pub first_seen_at: String,
    pub last_seen_at: String,
}

pub async fn record_search_query(db: &Db, plan_id: i64, key: &str, query: &str, engine: &str, depth: i64) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO search_query (plan_id,query_key,query,engine,hits,max_depth) VALUES ($1,$2,$3,$4,1,$5)
           ON CONFLICT (plan_id,query_key) DO UPDATE SET hits = search_query.hits + 1,
             max_depth = GREATEST(search_query.max_depth, excluded.max_depth),
             engine = CASE WHEN excluded.engine <> '' THEN excluded.engine ELSE search_query.engine END,
             last_used_at = now()"#,
    )
    .bind(plan_id)
    .bind(key)
    .bind(query)
    .bind(engine)
    .bind(depth.max(1) as i32)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn record_visited_page(db: &Db, plan_id: i64, key: &str, url: &str, host: &str) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO visited_page (plan_id,url_key,url,host,visits) VALUES ($1,$2,$3,$4,1)
           ON CONFLICT (plan_id,url_key) DO UPDATE SET visits = visited_page.visits + 1, last_seen_at = now()"#,
    )
    .bind(plan_id)
    .bind(key)
    .bind(url)
    .bind(host)
    .execute(db)
    .await?;
    Ok(())
}

/// Splits an iteration's new prospects across the queries it used.
pub async fn attribute_query_yield_inner(db: &Db, plan_id: i64, keys: &[String], new_prospects: i64) -> Result<()> {
    if keys.is_empty() || new_prospects <= 0 {
        return Ok(());
    }
    let n = keys.len() as i64;
    let each = new_prospects / n;
    let mut remainder = new_prospects % n;
    for key in keys {
        let mut share = each;
        if remainder > 0 {
            share += 1;
            remainder -= 1;
        }
        if share == 0 {
            continue;
        }
        sqlx::query(r#"UPDATE search_query SET new_prospects = new_prospects + $3 WHERE plan_id=$1 AND query_key=$2"#)
            .bind(plan_id)
            .bind(key)
            .bind(share as i32)
            .execute(db)
            .await?;
    }
    Ok(())
}

pub async fn list_search_queries(db: &Db, plan_id: i64, limit: i64) -> Result<Vec<SearchQueryRow>> {
    let rows = sqlx::query(
        r#"SELECT query_key,query,engine,hits,max_depth,new_prospects,first_used_at,last_used_at,tokens
           FROM search_query WHERE plan_id=$1 ORDER BY hits DESC, last_used_at DESC LIMIT $2"#,
    )
    .bind(plan_id)
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| SearchQueryRow {
            query_key: r.get(0),
            query: r.get(1),
            engine: r.get(2),
            hits: r.get::<i32, _>(3) as i64,
            max_depth: r.get::<i32, _>(4) as i64,
            new_prospects: r.get::<i32, _>(5) as i64,
            first_used_at: ts(r.get(6)),
            last_used_at: ts(r.get(7)),
            tokens: r.get(8),
        })
        .collect())
}

pub async fn list_visited_pages(db: &Db, plan_id: i64, limit: i64) -> Result<Vec<VisitedPageRow>> {
    let rows = sqlx::query(
        r#"SELECT url_key,url,host,visits,first_seen_at,last_seen_at FROM visited_page
           WHERE plan_id=$1 ORDER BY last_seen_at ASC LIMIT $2"#,
    )
    .bind(plan_id)
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| VisitedPageRow {
            url_key: r.get(0),
            url: r.get(1),
            host: r.get(2),
            visits: r.get::<i32, _>(3) as i64,
            first_seen_at: ts(r.get(4)),
            last_seen_at: ts(r.get(5)),
        })
        .collect())
}

pub async fn prune_search_trail(db: &Db, plan_id: i64) -> Result<()> {
    sqlx::query(
        r#"DELETE FROM search_query WHERE plan_id=$1 AND query_key NOT IN
           (SELECT query_key FROM search_query WHERE plan_id=$1 ORDER BY last_used_at DESC LIMIT $2)"#,
    )
    .bind(plan_id)
    .bind(MAX_QUERIES_PER_PLAN)
    .execute(db)
    .await?;
    sqlx::query(
        r#"DELETE FROM visited_page WHERE plan_id=$1 AND url_key NOT IN
           (SELECT url_key FROM visited_page WHERE plan_id=$1 ORDER BY last_seen_at DESC LIMIT $2)"#,
    )
    .bind(plan_id)
    .bind(MAX_PAGES_PER_PLAN)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn search_trail_counts(db: &Db, plan_id: i64) -> Result<(i64, i64)> {
    let row = sqlx::query(
        r#"SELECT (SELECT count(*) FROM search_query WHERE plan_id=$1), (SELECT count(*) FROM visited_page WHERE plan_id=$1)"#,
    )
    .bind(plan_id)
    .fetch_one(db)
    .await?;
    Ok((row.get(0), row.get(1)))
}

// ---------------------------------------------------------------------------
// Prompt variants
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptVariant {
    pub prompt_hash: String,
    pub prompt: String,
    pub origin: String,
    pub created_at: String,
    pub executions: i64,
    pub new_prospects: i64,
}

pub async fn save_prompt_variant(db: &Db, plan_id: i64, hash: &str, prompt: &str, origin: &str) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO prompt_variant (plan_id,prompt_hash,prompt,origin) VALUES ($1,$2,$3,$4)
           ON CONFLICT DO NOTHING"#,
    )
    .bind(plan_id)
    .bind(hash)
    .bind(prompt)
    .bind(origin)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn load_prompt_variant(db: &Db, plan_id: i64, hash: &str) -> Result<Option<PromptVariant>> {
    let row = sqlx::query(
        r#"SELECT prompt_hash,prompt,origin,created_at,runs,new_prospects FROM prompt_variant
           WHERE plan_id=$1 AND prompt_hash=$2"#,
    )
    .bind(plan_id)
    .bind(hash)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| variant_from(&r)))
}

fn variant_from(r: &sqlx::postgres::PgRow) -> PromptVariant {
    PromptVariant {
        prompt_hash: r.get::<String, _>(0).trim().to_string(),
        prompt: r.get(1),
        origin: r.get(2),
        created_at: ts(r.get(3)),
        executions: r.get::<i32, _>(4) as i64,
        new_prospects: r.get::<i32, _>(5) as i64,
    }
}

pub async fn record_variant_result(db: &Db, plan_id: i64, hash: &str, new_rows: i64) -> Result<()> {
    sqlx::query(
        r#"UPDATE prompt_variant SET runs = runs + 1, new_prospects = new_prospects + $3
           WHERE plan_id=$1 AND prompt_hash=$2"#,
    )
    .bind(plan_id)
    .bind(hash)
    .bind(new_rows as i32)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn list_prompt_variants(db: &Db, plan_id: i64) -> Result<Vec<PromptVariant>> {
    let rows = sqlx::query(
        r#"SELECT prompt_hash,prompt,origin,created_at,runs,new_prospects FROM prompt_variant
           WHERE plan_id=$1 ORDER BY new_prospects DESC, runs DESC, created_at DESC"#,
    )
    .bind(plan_id)
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(variant_from).collect())
}

/// One execution of a plan, joined to the plan's name so a run list needs one
/// query. `AccountId` is loaded (the pipeline meters against it) but never
/// serialized — a run's JSON goes to the browser.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ExecutionRecord {
    pub execution_id: i64,
    /// The remote browser session, when there is one. Never sent to a browser:
    /// it is the key to a viewing URL, handed out only after an ownership check.
    #[serde(skip)]
    pub browser_session_id: String,
    pub plan_id: i64,
    #[serde(skip)]
    pub account_id: i64,
    /// The plan's name, from the join.
    pub source: String,
    pub status: String,
    pub trigger: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
    pub args_json: Value,
    pub new_prospects: i32,
    /// Billable tokens this run spent — input + output, cache reads excluded
    /// (they are the provider's discount, not new work).
    pub input_tokens: i64,
    pub output_tokens: i64,
    #[serde(skip)]
    pub pid: Option<i32>,
    /// The DevTools port this run's Chrome was given, set at creation.
    #[serde(skip)]
    pub cdp_port: Option<i32>,
}

impl ExecutionRecord {
    /// What this run is billed at: tokens valued at the account's sell rate.
    /// The customer's number, not our cost.
    pub fn cost_usd(&self) -> f64 {
        (self.input_tokens + self.output_tokens) as f64 * crate::config::sell_usd_per_mtoken() / 1e6
    }

    /// What each thing it found cost. `None` when it found nothing — which is
    /// the case worth seeing, so it is a distinct answer rather than infinity.
    pub fn cost_per_result(&self) -> Option<f64> {
        (self.new_prospects > 0).then(|| self.cost_usd() / self.new_prospects as f64)
    }
}

const EXECUTION_COLS: &str = r#"r.execution_id,r.plan_id,r.account_id,p.source,r.status,r.trigger,r.started_at,r.browser_session_id,
r.finished_at,r.exit_code,r.args_json,r.new_prospects,r.input_tokens,r.output_tokens,r.pid,r.cdp_port"#;

pub async fn create_execution(db: &Db, account_id: i64, plan_id: i64, trigger: &str, args: &Value, cdp_port: u16) -> Result<i64> {
    Ok(sqlx::query_scalar(
        r#"INSERT INTO execution (plan_id,account_id,trigger,args_json,cdp_port) VALUES ($1,$2,$3,$4,$5) RETURNING execution_id"#,
    )
    .bind(plan_id)
    .bind(account_id)
    .bind(trigger)
    .bind(args)
    .bind(cdp_port as i32)
    .fetch_one(db)
    .await?)
}

pub async fn set_execution_running(db: &Db, execution_id: i64, pid: Option<u32>) -> Result<()> {
    sqlx::query(r#"UPDATE execution SET status='running', pid=$2 WHERE execution_id=$1 AND status='queued'"#)
        .bind(execution_id)
        .bind(pid.map(|p| p as i32))
        .execute(db)
        .await?;
    Ok(())
}

pub async fn finish_execution(db: &Db, execution_id: i64, status: &str, exit_code: Option<i32>) -> Result<()> {
    let n = sqlx::query(
        r#"UPDATE execution SET status=$2, exit_code=$3, finished_at=now() WHERE execution_id=$1 AND status IN ('queued','running')"#,
    )
    .bind(execution_id)
    .bind(status)
    .bind(exit_code)
    .execute(db)
    .await?
    .rows_affected();
    // Only when this call is the one that finished it. The WHERE clause makes
    // finishing first-writer-wins — a worker and a reaper can both call this for
    // the same run — and an event per caller would announce the run twice.
    if n > 0 {
        let (account_id, plan_id) = execution_owner(db, execution_id).await.unwrap_or((None, None));
        crate::bus::publish(
            crate::bus::subject::RUN_FINISHED,
            account_id,
            serde_json::json!({
                "execution_id": execution_id,
                "plan_id": plan_id,
                "status": status,
                "exit_code": exit_code,
            }),
        )
        .await;
    }
    Ok(())
}

/// Who an execution belongs to, for putting an account on an event published
/// from a function that was only given the execution id.
async fn execution_owner(db: &Db, execution_id: i64) -> Result<(Option<i64>, Option<i64>)> {
    let row = sqlx::query(r#"SELECT account_id,plan_id FROM execution WHERE execution_id=$1"#)
        .bind(execution_id)
        .fetch_optional(db)
        .await?;
    Ok(row.map(|r| (r.get::<Option<i64>, _>("account_id"), r.get::<Option<i64>, _>("plan_id"))).unwrap_or((None, None)))
}

/// Records which remote browser session this execution is driving.
pub async fn set_execution_browser_session(db: &Db, execution_id: i64, session_id: &str) -> Result<()> {
    sqlx::query(r#"UPDATE execution SET browser_session_id=$2 WHERE execution_id=$1"#)
        .bind(execution_id)
        .bind(session_id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn set_execution_new_prospects(db: &Db, execution_id: i64, n: i64) -> Result<()> {
    sqlx::query(r#"UPDATE execution SET new_prospects=$2 WHERE execution_id=$1"#)
        .bind(execution_id)
        .bind(n as i32)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn get_execution(db: &Db, account_id: i64, execution_id: i64) -> Result<Option<ExecutionRecord>> {
    let row = sqlx::query(&format!(
        r#"SELECT {EXECUTION_COLS} FROM execution r JOIN plan p ON p.plan_id=r.plan_id WHERE r.account_id=$1 AND r.execution_id=$2"#
    ))
    .bind(account_id)
    .bind(execution_id)
    .fetch_optional(db)
    .await?;
    row.map(|r| ExecutionRecord::from_row(&r).map_err(Into::into)).transpose()
}

pub async fn get_execution_unscoped(db: &Db, execution_id: i64) -> Result<Option<ExecutionRecord>> {
    let row = sqlx::query(&format!(
        r#"SELECT {EXECUTION_COLS} FROM execution r JOIN plan p ON p.plan_id=r.plan_id WHERE r.execution_id=$1"#
    ))
    .bind(execution_id)
    .fetch_optional(db)
    .await?;
    row.map(|r| ExecutionRecord::from_row(&r).map_err(Into::into)).transpose()
}

pub async fn list_executions(db: &Db, account_id: i64, plan_id: Option<i64>, limit: i64) -> Result<Vec<ExecutionRecord>> {
    let rows = sqlx::query(&format!(
        r#"SELECT {EXECUTION_COLS} FROM execution r JOIN plan p ON p.plan_id=r.plan_id
           WHERE r.account_id=$1 AND ($2::bigint IS NULL OR r.plan_id=$2) ORDER BY r.execution_id DESC LIMIT $3"#
    ))
    .bind(account_id)
    .bind(plan_id)
    .bind(limit.clamp(1, 500))
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(ExecutionRecord::from_row).collect::<Result<Vec<_>, _>>()?)
}

pub async fn active_execution_for_plan(db: &Db, plan_id: i64) -> Result<Option<ExecutionRecord>> {
    let row = sqlx::query(&format!(
        r#"SELECT {EXECUTION_COLS} FROM execution r JOIN plan p ON p.plan_id=r.plan_id
           WHERE r.plan_id=$1 AND r.status IN ('queued','running') ORDER BY r.execution_id DESC LIMIT 1"#
    ))
    .bind(plan_id)
    .fetch_optional(db)
    .await?;
    row.map(|r| ExecutionRecord::from_row(&r).map_err(Into::into)).transpose()
}

/// Runs left 'running' by a server that died. Called at startup, before the
/// scheduler can be fooled into thinking those plans are busy.
pub async fn abandon_stale_executions(db: &Db) -> Result<u64> {
    Ok(sqlx::query(
        r#"UPDATE execution SET status='failed', finished_at=now(), exit_code=-1 WHERE status IN ('queued','running')"#,
    )
    .execute(db)
    .await?
    .rows_affected())
}

pub async fn append_execution_log(db: &Db, execution_id: i64, stream: &str, line: &str) -> Result<i64> {
    let seq: i64 = sqlx::query_scalar(
        r#"INSERT INTO execution_log (execution_id,seq,stream,line)
           SELECT $1, coalesce(max(seq),0)+1, $2, $3 FROM execution_log WHERE execution_id=$1 RETURNING seq"#,
    )
    .bind(execution_id)
    .bind(stream)
    .bind(line)
    .fetch_one(db)
    .await
    .map(|s: i32| s as i64)?;
    Ok(seq)
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecutionLogLine {
    pub seq: i64,
    pub ts: String,
    pub stream: String,
    pub line: String,
}

pub async fn list_execution_logs(db: &Db, execution_id: i64, after_seq: i64, limit: i64) -> Result<Vec<ExecutionLogLine>> {
    let rows = sqlx::query(
        r#"SELECT seq,ts,stream,line FROM execution_log WHERE execution_id=$1 AND seq > $2 ORDER BY seq LIMIT $3"#,
    )
    .bind(execution_id)
    .bind(after_seq as i32)
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| ExecutionLogLine {
            seq: r.get::<i32, _>(0) as i64,
            ts: ts(r.get(1)),
            stream: r.get(2),
            line: r.get(3),
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Schedules
// ---------------------------------------------------------------------------

pub fn normalize_schedule_time(time: &str) -> Result<String> {
    let t = time.trim();
    if t.is_empty() {
        return Ok(String::new());
    }
    let parsed = NaiveTime::parse_from_str(t, "%H:%M").map_err(|_| anyhow!("schedule time must be HH:MM, got {t:?}"))?;
    Ok(parsed.format("%H:%M").to_string())
}

pub fn normalize_schedule_days(days: &str) -> Result<String> {
    let mut out: Vec<u32> = Vec::new();
    for part in days.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let d: u32 = part.parse().map_err(|_| anyhow!("schedule day must be 0-6, got {part:?}"))?;
        if d > 6 {
            bail!("schedule day must be 0-6, got {d}");
        }
        if !out.contains(&d) {
            out.push(d);
        }
    }
    out.sort_unstable();
    if out.len() == 7 {
        return Ok(String::new());
    }
    Ok(out.iter().map(|d| d.to_string()).collect::<Vec<_>>().join(","))
}

/// The next UTC instant a plan's local schedule fires after `now`.
pub fn next_fire(time: &str, days: &str, tz_name: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let t = NaiveTime::parse_from_str(time.trim(), "%H:%M").ok()?;
    let tz: chrono_tz::Tz = tz_name.parse().unwrap_or(chrono_tz::UTC);
    let allowed: Vec<u32> = days.split(',').filter_map(|d| d.trim().parse().ok()).collect();
    let local_now = now.with_timezone(&tz);
    for offset in 0..8 {
        let day = local_now.date_naive() + Duration::days(offset);
        let weekday = day.weekday().num_days_from_sunday();
        if !allowed.is_empty() && !allowed.contains(&weekday) {
            continue;
        }
        let candidate = tz.from_local_datetime(&day.and_time(t)).earliest()?;
        if candidate > local_now {
            return Some(candidate.with_timezone(&Utc));
        }
    }
    None
}

pub async fn refresh_schedule(db: &Db, plan_id: i64) -> Result<Option<DateTime<Utc>>> {
    let row = sqlx::query(
        r#"SELECT schedule_enabled, schedule_time, schedule_days, account_id
           FROM plan WHERE plan_id=$1"#,
    )
    .bind(plan_id)
    .fetch_optional(db)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let enabled: bool = row.get(0);
    let time: String = row.get(1);
    let days: String = row.get(2);
    let account_id: i64 = row.get(3);
    // The schedule is a local wall-clock, so it needs the account's timezone —
    // which lives in the `Account` table (the auth service's own database in the
    // hosted split). A service that cannot see that table falls back to UTC; a
    // scheduled plan is re-based to the real zone when the user next saves it.
    let tz: String = sqlx::query_scalar::<_, String>(r#"SELECT timezone FROM account WHERE account_id=$1"#)
        .bind(account_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "UTC".into());
    let next = if enabled && !time.is_empty() { next_fire(&time, &days, &tz, Utc::now()) } else { None };
    sqlx::query(r#"UPDATE plan SET next_run_at=$2 WHERE plan_id=$1"#)
        .bind(plan_id)
        .bind(next)
        .execute(db)
        .await?;
    Ok(next)
}

pub async fn refresh_all_schedules(db: &Db) -> Result<usize> {
    let ids: Vec<i64> = sqlx::query_scalar(r#"SELECT plan_id FROM plan WHERE schedule_enabled"#)
        .fetch_all(db)
        .await?;
    let n = ids.len();
    for id in ids {
        refresh_schedule(db, id).await?;
    }
    Ok(n)
}

pub async fn scheduled_plans_due(db: &Db) -> Result<Vec<(i64, i64)>> {
    let rows = sqlx::query(
        r#"SELECT plan_id,account_id FROM plan WHERE schedule_enabled AND next_run_at IS NOT NULL AND next_run_at <= now()
           ORDER BY next_run_at"#,
    )
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(|r| (r.get(0), r.get(1))).collect())
}

pub async fn mark_schedule_fired(db: &Db, plan_id: i64) -> Result<()> {
    sqlx::query(r#"UPDATE plan SET last_scheduled_at=now() WHERE plan_id=$1"#)
        .bind(plan_id)
        .execute(db)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// API keys
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ApiKeyRow {
    pub key_id: i64,
    /// Populated only by `create_api_key`, for the one response that shows it.
    pub token: String,
    pub token_hint: String,
    pub label: String,
    pub plan_id: Option<i64>,
    pub source: String,
    pub allow_cidr: String,
    pub expires_at: String,
    pub created_at: String,
    pub last_used_at: String,
    pub last_used_ip: String,
    pub uses: i64,
    pub revoked: bool,
    pub expired: bool,
    #[serde(skip)]
    pub account_id: i64,
}

pub fn new_api_token() -> String {
    let mut buf = [0u8; 24];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut buf);
    format!("psk_{}", hex::encode(buf))
}

const KEY_COLS: &str = r#"k.key_id, k.token_hint, k.label, k.plan_id, coalesce(p.source,'') AS source, k.allow_cidr,
k.expires_at, k.created_at, k.last_used_at, k.last_used_ip, k.uses, k.revoked, k.account_id,
(k.expires_at IS NOT NULL AND k.expires_at <= now()) AS expired"#;

fn key_from(r: &sqlx::postgres::PgRow) -> ApiKeyRow {
    ApiKeyRow {
        key_id: r.get("key_id"),
        token: String::new(),
        token_hint: r.get("token_hint"),
        label: r.get("label"),
        plan_id: r.get("plan_id"),
        source: r.get("source"),
        allow_cidr: r.get("allow_cidr"),
        expires_at: ts(r.get("expires_at")),
        created_at: ts(r.get("created_at")),
        last_used_at: ts(r.get("last_used_at")),
        last_used_ip: r.get("last_used_ip"),
        uses: r.get::<i32, _>("uses") as i64,
        revoked: r.get("revoked"),
        expired: r.get("expired"),
        account_id: r.get("account_id"),
    }
}

pub async fn create_api_key(
    db: &Db,
    account_id: i64,
    label: &str,
    plan_id: Option<i64>,
    expires_in_days: i64,
    allow_cidr: &str,
) -> Result<ApiKeyRow> {
    if let Some(pid) = plan_id {
        if get_plan(db, account_id, pid).await?.is_none() {
            bail!("plan not found");
        }
    }
    crate::cidr::parse_list(allow_cidr).map_err(|e| anyhow!("allowed addresses: {e}"))?;
    let token = new_api_token();
    let hash = sha256_hex(&token);
    let hint: String = token.chars().take(12).collect();
    let expires = (expires_in_days > 0).then(|| Utc::now() + Duration::days(expires_in_days));
    let key_id: i64 = sqlx::query_scalar(
        r#"INSERT INTO api_key (account_id,plan_id,token_hash,token_hint,label,allow_cidr,expires_at)
           VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING key_id"#,
    )
    .bind(account_id)
    .bind(plan_id)
    .bind(&hash)
    .bind(&hint)
    .bind(label.trim())
    .bind(allow_cidr.trim())
    .bind(expires)
    .fetch_one(db)
    .await?;
    let mut row = get_api_key(db, account_id, key_id).await?.ok_or_else(|| anyhow!("key vanished"))?;
    row.token = token;
    Ok(row)
}

pub async fn get_api_key(db: &Db, account_id: i64, key_id: i64) -> Result<Option<ApiKeyRow>> {
    let row = sqlx::query(&format!(
        r#"SELECT {KEY_COLS} FROM api_key k LEFT JOIN plan p ON p.plan_id=k.plan_id WHERE k.account_id=$1 AND k.key_id=$2"#
    ))
    .bind(account_id)
    .bind(key_id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| key_from(&r)))
}

pub async fn list_api_keys(db: &Db, account_id: i64) -> Result<Vec<ApiKeyRow>> {
    let rows = sqlx::query(&format!(
        r#"SELECT {KEY_COLS} FROM api_key k LEFT JOIN plan p ON p.plan_id=k.plan_id WHERE k.account_id=$1 ORDER BY k.key_id DESC"#
    ))
    .bind(account_id)
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(key_from).collect())
}

pub async fn revoke_api_key(db: &Db, account_id: i64, key_id: i64) -> Result<bool> {
    Ok(sqlx::query(r#"UPDATE api_key SET revoked=true WHERE account_id=$1 AND key_id=$2"#)
        .bind(account_id)
        .bind(key_id)
        .execute(db)
        .await?
        .rows_affected()
        > 0)
}

pub async fn delete_api_key(db: &Db, account_id: i64, key_id: i64) -> Result<bool> {
    Ok(sqlx::query(r#"DELETE FROM api_key WHERE account_id=$1 AND key_id=$2"#)
        .bind(account_id)
        .bind(key_id)
        .execute(db)
        .await?
        .rows_affected()
        > 0)
}

pub async fn set_api_key_cidr(db: &Db, account_id: i64, key_id: i64, allow_cidr: &str) -> Result<bool> {
    crate::cidr::parse_list(allow_cidr).map_err(|e| anyhow!("allowed addresses: {e}"))?;
    Ok(sqlx::query(r#"UPDATE api_key SET allow_cidr=$3 WHERE account_id=$1 AND key_id=$2"#)
        .bind(account_id)
        .bind(key_id)
        .bind(allow_cidr.trim())
        .execute(db)
        .await?
        .rows_affected()
        > 0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthFailure {
    UnknownToken,
    AddressNotAllowed,
}

/// Looks a presented token up by its hash. The hash lookup is an index probe
/// so timing reveals nothing about *which* key was close; every failure mode
/// maps to the same outward 401 by the caller.
pub async fn authenticate_api_key(db: &Db, token: &str, from: Option<IpAddr>) -> Result<Result<ApiKeyRow, AuthFailure>> {
    let hash = sha256_hex(token.trim());
    let row = sqlx::query(&format!(
        r#"SELECT {KEY_COLS} FROM api_key k LEFT JOIN plan p ON p.plan_id=k.plan_id
           WHERE k.token_hash=$1 AND NOT k.revoked AND (k.expires_at IS NULL OR k.expires_at > now())"#
    ))
    .bind(&hash)
    .fetch_optional(db)
    .await?;
    let Some(row) = row else { return Ok(Err(AuthFailure::UnknownToken)) };
    let key = key_from(&row);
    if !key.allow_cidr.trim().is_empty() {
        let list = crate::cidr::parse_list(&key.allow_cidr).unwrap_or_default();
        match from {
            Some(ip) if crate::cidr::allows(&list, ip) => {}
            _ => return Ok(Err(AuthFailure::AddressNotAllowed)),
        }
    }
    sqlx::query(r#"UPDATE api_key SET uses=uses+1, last_used_at=now(), last_used_ip=$2 WHERE key_id=$1"#)
        .bind(key.key_id)
        .bind(from.map(|ip| ip.to_string()).unwrap_or_default())
        .execute(db)
        .await?;
    Ok(Ok(key))
}

pub async fn record_api_audit(db: &Db, account_id: Option<i64>, key_id: Option<i64>, ip: &str, outcome: &str, path: &str) -> Result<()> {
    sqlx::query(r#"INSERT INTO api_audit (account_id,key_id,ip,outcome,path) VALUES ($1,$2,$3,$4,$5)"#)
        .bind(account_id)
        .bind(key_id)
        .bind(ip)
        .bind(outcome)
        .bind(path.chars().take(200).collect::<String>())
        .execute(db)
        .await?;
    if let Some(acc) = account_id {
        sqlx::query(
            r#"DELETE FROM api_audit WHERE account_id=$1 AND audit_id NOT IN
               (SELECT audit_id FROM api_audit WHERE account_id=$1 ORDER BY audit_id DESC LIMIT $2)"#,
        )
        .bind(acc)
        .bind(AUDIT_KEEP_ROWS)
        .execute(db)
        .await?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiAuditRow {
    pub audit_id: i64,
    pub at: String,
    pub key_id: Option<i64>,
    pub ip: String,
    pub outcome: String,
    pub path: String,
}

pub async fn list_api_audit(db: &Db, account_id: i64, limit: i64) -> Result<Vec<ApiAuditRow>> {
    let rows = sqlx::query(
        r#"SELECT audit_id,at,key_id,ip,outcome,path FROM api_audit WHERE account_id=$1 ORDER BY audit_id DESC LIMIT $2"#,
    )
    .bind(account_id)
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| ApiAuditRow {
            audit_id: r.get(0),
            at: ts(r.get(1)),
            key_id: r.get(2),
            ip: r.get(3),
            outcome: r.get(4),
            path: r.get(5),
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Usage metering and the budget cap
// ---------------------------------------------------------------------------

/// A snapshot of an account's dollar budget for the current period. Amounts are
/// US dollars; `tokens_used` is the raw meter and `cogs_usd` is our Cursor cost
/// (so margin = used − cogs).
#[derive(Debug, Clone, Serialize)]
pub struct Usage {
    pub budget_usd: f64,
    pub topups_usd: f64,
    pub available_usd: f64,
    pub used_usd: f64,
    pub remaining_usd: f64,
    pub tokens_used: i64,
    pub cogs_usd: f64,
    pub period_start: String,
}

/// Tokens reported by one agent call (Cursor's `usage` object).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsage {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
}

impl TokenUsage {
    /// The metered unit for the cap: input + output. Cache tokens are recorded
    /// on the run for transparency but not billed (they are Cursor's own
    /// read-cache discount, not new work).
    pub fn billable(&self) -> i64 {
        self.input + self.output
    }
    pub fn is_zero(&self) -> bool {
        self.input == 0 && self.output == 0 && self.cache_read == 0 && self.cache_write == 0
    }
    pub fn add(&mut self, o: TokenUsage) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
    }
    /// What of `self` has not already been booked. Used when a `result`
    /// event repeats a cumulative total we already counted turn by turn.
    pub fn saturating_sub(self, o: TokenUsage) -> TokenUsage {
        TokenUsage {
            input: (self.input - o.input).max(0),
            output: (self.output - o.output).max(0),
            cache_read: (self.cache_read - o.cache_read).max(0),
            cache_write: (self.cache_write - o.cache_write).max(0),
        }
    }
}

/// Ensures the account has a usage row and that its period is current — rolling
/// it over (zeroing usage and top-ups) when a month has elapsed — then returns
/// the current [`Usage`].
pub async fn ensure_usage(db: &Db, account_id: i64) -> Result<Usage> {
    sqlx::query(r#"INSERT INTO account_usage (account_id) VALUES ($1) ON CONFLICT (account_id) DO NOTHING"#)
        .bind(account_id)
        .execute(db)
        .await?;
    // Roll the period monthly: zero consumption and top-ups, keep the budget.
    sqlx::query(
        r#"UPDATE account_usage
           SET tokens_used=0, cost_usd_micros=0, topup_usd_micros=0, period_start=now(), updated_at=now()
           WHERE account_id=$1 AND period_start <= now() - interval '1 month'"#,
    )
    .bind(account_id)
    .execute(db)
    .await?;
    let row = sqlx::query(
        r#"SELECT budget_usd_micros,topup_usd_micros,tokens_used,cost_usd_micros,period_start
           FROM account_usage WHERE account_id=$1"#,
    )
    .bind(account_id)
    .fetch_one(db)
    .await?;
    let budget_usd = row.get::<i64, _>(0) as f64 / 1e6;
    let topups_usd = row.get::<i64, _>(1) as f64 / 1e6;
    let tokens_used: i64 = row.get(2);
    let cogs_usd = row.get::<i64, _>(3) as f64 / 1e6;
    let period_start: DateTime<Utc> = row.get(4);
    // The customer is billed for input+output tokens at the sell rate.
    let used_usd = tokens_used as f64 * crate::config::sell_usd_per_mtoken() / 1e6;
    let available_usd = budget_usd + topups_usd;
    Ok(Usage {
        budget_usd,
        topups_usd,
        available_usd,
        used_usd,
        remaining_usd: (available_usd - used_usd).max(0.0),
        tokens_used,
        cogs_usd,
        period_start: period_start.to_rfc3339(),
    })
}

/// True when the account has spent its dollar budget for the period — the check
/// the run dispatcher makes before starting any run (manual or scheduled).
pub async fn account_over_budget(db: &Db, account_id: i64) -> Result<bool> {
    let u = ensure_usage(db, account_id).await?;
    Ok(u.used_usd >= u.available_usd)
}

/// Records one agent call's tokens and Cursor cost against a run and the
/// account's period usage. Called live during a run, so the meter ticks up as
/// work happens.
pub async fn add_execution_tokens(db: &Db, execution_id: i64, account_id: i64, u: TokenUsage, cost_micros: i64) -> Result<()> {
    if u.is_zero() && cost_micros == 0 {
        return Ok(());
    }
    sqlx::query(
        r#"UPDATE execution SET
            input_tokens=input_tokens+$2, output_tokens=output_tokens+$3,
            cache_read_tokens=cache_read_tokens+$4, cache_write_tokens=cache_write_tokens+$5,
            cost_usd_micros=cost_usd_micros+$6
           WHERE execution_id=$1"#,
    )
    .bind(execution_id)
    .bind(u.input)
    .bind(u.output)
    .bind(u.cache_read)
    .bind(u.cache_write)
    .bind(cost_micros)
    .execute(db)
    .await?;
    add_account_usage(db, account_id, u, cost_micros).await
}

/// The number the live page shows. Absolute — a climbing estimate is replaced
/// by the billed figure so the two never stack. Does not touch the account:
/// estimates are not a charge.
pub async fn set_execution_token_totals(db: &Db, execution_id: i64, u: TokenUsage) -> Result<()> {
    sqlx::query(
        r#"UPDATE execution SET
            input_tokens=$2, output_tokens=$3,
            cache_read_tokens=$4, cache_write_tokens=$5
           WHERE execution_id=$1"#,
    )
    .bind(execution_id)
    .bind(u.input)
    .bind(u.output)
    .bind(u.cache_read)
    .bind(u.cache_write)
    .execute(db)
    .await?;
    Ok(())
}

/// Bills the account. Separate from the run row so a live estimate can move
/// the meter without charging for tokens Cursor has not reported yet.
pub async fn add_account_usage(db: &Db, account_id: i64, u: TokenUsage, cost_micros: i64) -> Result<()> {
    if u.is_zero() && cost_micros == 0 {
        return Ok(());
    }
    ensure_usage(db, account_id).await?;
    sqlx::query(
        r#"UPDATE account_usage SET tokens_used=tokens_used+$2, cost_usd_micros=cost_usd_micros+$3, updated_at=now()
           WHERE account_id=$1"#,
    )
    .bind(account_id)
    .bind(u.billable())
    .bind(cost_micros)
    .execute(db)
    .await?;
    Ok(())
}

/// The card on file, as much of it as is safe to keep. `None` when the account
/// has never completed the Stripe setup flow.
#[derive(Debug, Clone, Serialize)]
pub struct PaymentMethod {
    /// Stripe customer id (`cus_…`), or a mock handle in local development.
    pub payment_ref: String,
    pub brand: String,
    pub last4: String,
    pub added_at: Option<DateTime<Utc>>,
}

pub async fn payment_method(db: &Db, account_id: i64) -> Result<Option<PaymentMethod>> {
    let row = sqlx::query(
        r#"SELECT payment_ref,card_brand,card_last4,card_added_at FROM account_usage WHERE account_id=$1"#,
    )
    .bind(account_id)
    .fetch_optional(db)
    .await?;
    Ok(row.and_then(|r| {
        let last4: String = r.get(2);
        // A customer with no card attached is not a payment method: Stripe
        // creates the customer first and the card only lands on return.
        if last4.trim().is_empty() {
            return None;
        }
        Some(PaymentMethod {
            payment_ref: r.get(0),
            brand: r.get(1),
            last4,
            added_at: r.get(3),
        })
    }))
}

/// The Stripe customer handle, with or without a card attached. Separate from
/// [`payment_method`], which reports only a usable card.
pub async fn payment_ref(db: &Db, account_id: i64) -> Result<String> {
    let row = sqlx::query(r#"SELECT payment_ref FROM account_usage WHERE account_id=$1"#)
        .bind(account_id)
        .fetch_optional(db)
        .await?;
    Ok(row.map(|r| r.get::<String, _>(0)).unwrap_or_default())
}

/// Whether a run may start. Deliberately its own query rather than a field on
/// [`Usage`]: the gate runs on every start and wants one cheap answer.
pub async fn has_payment_method(db: &Db, account_id: i64) -> Result<bool> {
    let row = sqlx::query(r#"SELECT card_last4 FROM account_usage WHERE account_id=$1"#)
        .bind(account_id)
        .fetch_optional(db)
        .await?;
    Ok(row.map(|r| !r.get::<String, _>(0).trim().is_empty()).unwrap_or(false))
}

/// Records the Stripe customer before the card exists, so the handle survives
/// an abandoned checkout and the next attempt reuses it.
pub async fn set_payment_ref(db: &Db, account_id: i64, payment_ref: &str) -> Result<()> {
    ensure_usage(db, account_id).await?;
    sqlx::query(r#"UPDATE account_usage SET payment_ref=$2, updated_at=now() WHERE account_id=$1"#)
        .bind(account_id)
        .bind(payment_ref)
        .execute(db)
        .await?;
    Ok(())
}

/// Records the card Stripe attached. Only the brand and last four ever arrive
/// here — the number itself never touches this process.
pub async fn set_payment_method(db: &Db, account_id: i64, payment_ref: &str, brand: &str, last4: &str) -> Result<()> {
    ensure_usage(db, account_id).await?;
    sqlx::query(
        r#"UPDATE account_usage
           SET payment_ref=$2, card_brand=$3, card_last4=$4, card_added_at=now(), updated_at=now()
           WHERE account_id=$1"#,
    )
    .bind(account_id)
    .bind(payment_ref)
    .bind(brand.chars().take(24).collect::<String>())
    .bind(last4.chars().rev().take(4).collect::<String>().chars().rev().collect::<String>())
    .execute(db)
    .await?;
    Ok(())
}

/// Adds purchased budget (micro-USD) for the current period — the payment hook.
pub async fn add_topup(db: &Db, account_id: i64, usd_micros: i64) -> Result<Usage> {
    ensure_usage(db, account_id).await?;
    sqlx::query(r#"UPDATE account_usage SET topup_usd_micros=topup_usd_micros+$2, updated_at=now() WHERE account_id=$1"#)
        .bind(account_id)
        .bind(usd_micros.max(0))
        .execute(db)
        .await?;
    ensure_usage(db, account_id).await
}

/// Sets the monthly budget in micro-USD (a plan-tier change). CLI/admin path.
#[allow(dead_code)]
pub async fn set_budget(db: &Db, account_id: i64, usd_micros: i64) -> Result<()> {
    ensure_usage(db, account_id).await?;
    sqlx::query(r#"UPDATE account_usage SET budget_usd_micros=$2, updated_at=now() WHERE account_id=$1"#)
        .bind(account_id)
        .bind(usd_micros.max(0))
        .execute(db)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Per-account browser identity (Browserbase Context + connected logins)
// ---------------------------------------------------------------------------

pub async fn ensure_account_browser(db: &Db, account_id: i64) -> Result<()> {
    sqlx::query(r#"INSERT INTO account_browser (account_id) VALUES ($1) ON CONFLICT (account_id) DO NOTHING"#)
        .bind(account_id)
        .execute(db)
        .await?;
    Ok(())
}

/// May this workspace connect an authenticated session?
///
/// Read from the row every time rather than cached: turning it off for a
/// workspace has to take effect on the next run, not on the next restart.
pub async fn connected_logins_enabled(db: &Db, account_id: i64) -> bool {
    sqlx::query_scalar::<_, bool>(r#"SELECT connected_logins FROM account WHERE account_id=$1"#)
        .bind(account_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
}

/// Turn the feature on or off for one workspace. Operator-only.
pub async fn set_connected_logins(db: &Db, account_id: i64, on: bool) -> Result<()> {
    sqlx::query(r#"UPDATE account SET connected_logins=$2 WHERE account_id=$1"#)
        .bind(account_id)
        .bind(on)
        .execute(db)
        .await?;
    Ok(())
}

/// The account's Browserbase Context id, if one has been created and the
/// workspace is allowed to use it.
///
/// The permission check lives here rather than at each call site: this is the
/// only way a stored context reaches a run, so a workspace that has the feature
/// switched off stops scraping logged-in immediately — even though its context
/// still exists and would otherwise still work.
pub async fn account_context(db: &Db, account_id: i64) -> Result<Option<String>> {
    if !connected_logins_enabled(db, account_id).await {
        return Ok(None);
    }
    let ctx: Option<String> = sqlx::query_scalar(r#"SELECT context_id FROM account_browser WHERE account_id = $1"#)
        .bind(account_id)
        .fetch_optional(db)
        .await?;
    Ok(ctx.filter(|c| !c.trim().is_empty()))
}

pub async fn set_account_context(db: &Db, account_id: i64, context_id: &str) -> Result<()> {
    ensure_account_browser(db, account_id).await?;
    sqlx::query(r#"UPDATE account_browser SET context_id=$2, updated_at=now() WHERE account_id=$1"#)
        .bind(account_id)
        .bind(context_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Records that the account connected a login for `site` (dedupes by site).
pub async fn record_browser_connection(db: &Db, account_id: i64, site: &str, url: &str) -> Result<()> {
    ensure_account_browser(db, account_id).await?;
    let current: String = sqlx::query_scalar(r#"SELECT connections_json FROM account_browser WHERE account_id=$1"#)
        .bind(account_id)
        .fetch_one(db)
        .await
        .unwrap_or_else(|_| "[]".into());
    let mut list: Vec<Value> = serde_json::from_str(&current).unwrap_or_default();
    list.retain(|c| c.get("site").and_then(Value::as_str) != Some(site));
    list.push(serde_json::json!({ "site": site, "url": url, "connected_at": Utc::now().to_rfc3339() }));
    sqlx::query(r#"UPDATE account_browser SET connections_json=$2, updated_at=now() WHERE account_id=$1"#)
        .bind(account_id)
        .bind(serde_json::to_string(&list).unwrap_or_else(|_| "[]".into()))
        .execute(db)
        .await?;
    Ok(())
}

pub async fn list_browser_connections(db: &Db, account_id: i64) -> Result<Vec<Value>> {
    let current: Option<String> = sqlx::query_scalar(r#"SELECT connections_json FROM account_browser WHERE account_id=$1"#)
        .bind(account_id)
        .fetch_optional(db)
        .await?;
    Ok(current.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default())
}

// ---------------------------------------------------------------------------
// Control plane: hosts, pool dispatch, admin operators
//
// These tables are operator-scoped, not tenant-scoped — no account_id (the
// documented exception to the tenancy rule). Placement columns live on execution
// so assignment and status stay one row.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Host {
    pub host_id: i64,
    pub name: String,
    /// Never serialized to the browser.
    #[serde(skip_serializing)]
    pub kubeconfig_yaml: String,
    pub kube_context: Option<String>,
    pub enabled: bool,
    pub pool_size: i32,
    pub cpu_request: String,
    pub cpu_limit: String,
    pub mem_request: String,
    pub mem_limit: String,
    pub image: String,
    /// Deploy the application services to this cluster as well as the pool.
    pub runs_services: bool,
    /// Website replicas, when this host runs the services.
    pub web_replicas: i32,
    pub notes: String,
    pub last_error: Option<String>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

const HOST_COLS: &str = r#"host_id,name,kubeconfig_yaml,kube_context,enabled,pool_size,
cpu_request,cpu_limit,mem_request,mem_limit,image,runs_services,web_replicas,notes,
last_error,last_seen_at,created_at"#;

pub async fn create_host(db: &Db, h: &Host) -> Result<i64> {
    Ok(sqlx::query_scalar(
        r#"INSERT INTO host (name,kubeconfig_yaml,kube_context,enabled,pool_size,
           cpu_request,cpu_limit,mem_request,mem_limit,image,runs_services,web_replicas,notes)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13) RETURNING host_id"#,
    )
    .bind(&h.name)
    .bind(&h.kubeconfig_yaml)
    .bind(&h.kube_context)
    .bind(h.enabled)
    .bind(h.pool_size)
    .bind(&h.cpu_request)
    .bind(&h.cpu_limit)
    .bind(&h.mem_request)
    .bind(&h.mem_limit)
    .bind(&h.image)
    .bind(h.runs_services)
    .bind(h.web_replicas)
    .bind(&h.notes)
    .fetch_one(db)
    .await?)
}

pub async fn update_host(db: &Db, h: &Host) -> Result<()> {
    // Empty kubeconfig on update = keep the stored one (the UI never round-trips it).
    sqlx::query(
        r#"UPDATE host SET name=$2, kubeconfig_yaml=CASE WHEN $3='' THEN kubeconfig_yaml ELSE $3 END,
           kube_context=$4, enabled=$5, pool_size=$6, cpu_request=$7, cpu_limit=$8,
           mem_request=$9, mem_limit=$10, image=$11, runs_services=$12, web_replicas=$13,
           notes=$14, updated_at=now()
           WHERE host_id=$1"#,
    )
    .bind(h.host_id)
    .bind(&h.name)
    .bind(&h.kubeconfig_yaml)
    .bind(&h.kube_context)
    .bind(h.enabled)
    .bind(h.pool_size)
    .bind(&h.cpu_request)
    .bind(&h.cpu_limit)
    .bind(&h.mem_request)
    .bind(&h.mem_limit)
    .bind(&h.image)
    .bind(h.runs_services)
    .bind(h.web_replicas)
    .bind(&h.notes)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn list_hosts(db: &Db) -> Result<Vec<Host>> {
    let rows = sqlx::query(&format!(r#"SELECT {HOST_COLS} FROM host ORDER BY host_id"#))
        .fetch_all(db)
        .await?;
    Ok(rows.iter().map(Host::from_row).collect::<Result<Vec<_>, _>>()?)
}

pub async fn get_host(db: &Db, host_id: i64) -> Result<Option<Host>> {
    let row = sqlx::query(&format!(r#"SELECT {HOST_COLS} FROM host WHERE host_id=$1"#))
        .bind(host_id)
        .fetch_optional(db)
        .await?;
    row.map(|r| Host::from_row(&r).map_err(Into::into)).transpose()
}

/// Refuses while runs still reference the host and are not terminal, so run
/// history keeps a meaningful host_id even though there is no FK.
pub async fn delete_host(db: &Db, host_id: i64) -> Result<()> {
    let active: i64 =
        sqlx::query_scalar(r#"SELECT count(*) FROM execution WHERE host_id=$1 AND status IN ('queued','running')"#)
            .bind(host_id)
            .fetch_one(db)
            .await?;
    if active > 0 {
        anyhow::bail!("host has {active} active run(s) — cancel them first");
    }
    sqlx::query(r#"DELETE FROM host WHERE host_id=$1"#).bind(host_id).execute(db).await?;
    Ok(())
}

/// `err=None` marks the host healthy (clears LastError, bumps LastSeenAt).
pub async fn set_host_health(db: &Db, host_id: i64, err: Option<&str>) -> Result<()> {
    sqlx::query(r#"UPDATE host SET last_error=$2, last_seen_at=CASE WHEN $2 IS NULL THEN now() ELSE last_seen_at END WHERE host_id=$1"#)
        .bind(host_id)
        .bind(err)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn get_setting(db: &Db, key: &str) -> Result<Option<String>> {
    Ok(sqlx::query_scalar(r#"SELECT value FROM control_setting WHERE key=$1"#)
        .bind(key)
        .fetch_optional(db)
        .await?)
}

/// The model chosen for each agent stage, as set in the admin console.
///
/// Missing keys simply do not appear, which is what leaves that stage on the
/// CLI default. Read once per process — see `agent::set_stage_models`.
/// The kinds every new account gets. Tables and people are the product;
/// reports and files are still experimental, so they are off unless somebody
/// turns them on.
pub const DEFAULT_KINDS: [&str; 2] = ["prospects", "artifacts"];

/// The settings key holding the installation-wide default.
pub const KINDS_SETTING: &str = "kinds_default";

/// Parses a stored kind list, keeping only kinds that exist.
pub fn parse_kinds(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for piece in raw.split([',', ' ', '\n', '\t']) {
        let p = piece.trim().to_ascii_lowercase();
        if p.is_empty() {
            continue;
        }
        // parse() maps anything unknown to prospects, so check the round trip
        // rather than trusting the input.
        let k = PlanKind::parse(&p);
        if k.as_str() == p && !out.iter().any(|x| x == &p) {
            out.push(p);
        }
    }
    out
}

/// Which plan kinds an account may create.
///
/// Account setting first, then the installation default, then the built-in.
/// An account row that says nothing is the normal case: it means "whatever the
/// installation currently allows", so enabling a feature for everyone is one
/// settings write rather than an update over every row.
pub async fn allowed_kinds(db: &Db, account_id: i64) -> Vec<String> {
    if let Ok(Some(acc)) = get_account(db, account_id).await {
        let own = parse_kinds(&acc.enabled_kinds);
        if !own.is_empty() {
            return own;
        }
    }
    installation_kinds(db).await
}

/// The installation-wide default, for accounts that have no say of their own.
pub async fn installation_kinds(db: &Db) -> Vec<String> {
    if let Ok(Some(raw)) = get_setting(db, KINDS_SETTING).await {
        let parsed = parse_kinds(&raw);
        if !parsed.is_empty() {
            return parsed;
        }
    }
    DEFAULT_KINDS.iter().map(|s| s.to_string()).collect()
}

/// Sets one account's kinds. Empty puts it back on the installation default.
/// The wording an acknowledgement is against. Change it and every existing
/// acceptance keeps the text it was given, which is the point of storing it.
pub const PLATFORM_ACK_TEXT: &str = "I have the right to collect data from LinkedIn, Facebook, Instagram, X and Threads, \
and I accept responsibility for doing so, including any claim brought by those platforms.";

/// Records (or withdraws) the platform acknowledgement for a **workspace**.
///
/// Stored against the workspace, not the person: it governs the plans and the
/// data, both of which belong to the workspace. `by` is the individual who
/// clicked, kept because a dispute asks who accepted, not just whether someone
/// did. Withdrawing clears all three, so a run started afterwards refuses those
/// hosts again.
pub async fn set_platform_ack(db: &Db, workspace_id: i64, by: i64, on: bool) -> Result<()> {
    sqlx::query(
        r#"UPDATE account SET
             platform_ack_at   = CASE WHEN $3 THEN now() ELSE NULL END,
             platform_ack_by   = CASE WHEN $3 THEN $2 ELSE NULL END,
             platform_ack_text = CASE WHEN $3 THEN $4 ELSE '' END
           WHERE account_id=$1"#,
    )
    .bind(workspace_id)
    .bind(by)
    .bind(on)
    .bind(PLATFORM_ACK_TEXT)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn set_account_kinds(db: &Db, account_id: i64, kinds: &str) -> Result<()> {
    sqlx::query(r#"UPDATE account SET enabled_kinds=$2 WHERE account_id=$1"#)
        .bind(account_id)
        .bind(parse_kinds(kinds).join(","))
        .execute(db)
        .await?;
    Ok(())
}

/// Accounts for the admin console: who exists, and what they may build.
pub async fn list_accounts_brief(db: &Db, limit: i64) -> Result<Vec<Value>> {
    let rows = sqlx::query(
        r#"SELECT a.account_id, a.email, a.display_name, a.enabled_kinds, a.connected_logins, a.created_at,
                  (SELECT count(*) FROM plan p WHERE p.account_id = a.account_id) AS plans
           FROM account a ORDER BY a.account_id DESC LIMIT $1"#,
    )
    .bind(limit.clamp(1, 500))
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "account_id": r.get::<i64, _>("account_id"),
                "email": r.get::<String, _>("email"),
                "display_name": r.get::<String, _>("display_name"),
                "kinds": r.get::<String, _>("enabled_kinds"),
                "connected_logins": r.get::<bool, _>("connected_logins"),
                "plans": r.get::<i64, _>("plans"),
                "created_at": r.get::<DateTime<Utc>, _>("created_at").to_rfc3339(),
            })
        })
        .collect())
}

pub async fn stage_models(db: &Db) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (stage, key) in crate::agent::STAGES {
        if let Ok(Some(v)) = get_setting(db, key).await {
            if !v.trim().is_empty() {
                out.insert(stage.to_string(), v);
            }
        }
    }
    out
}

pub async fn set_setting(db: &Db, key: &str, value: &str) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO control_setting (key,value) VALUES ($1,$2)
           ON CONFLICT (key) DO UPDATE SET value=excluded.value, updated_at=now()"#,
    )
    .bind(key)
    .bind(value)
    .execute(db)
    .await?;
    Ok(())
}

/// How many operators exist. Zero means the install has not been claimed.
pub async fn count_admin_users(db: &Db) -> Result<i64> {
    Ok(sqlx::query_scalar(r#"SELECT count(*) FROM admin_user"#).fetch_one(db).await?)
}

/// What the control plane can see, for the banner it prints at boot.
///
/// An operator's first question on a new install is "is it looking at the right
/// database", and the answer should not require them to go and ask Postgres
/// themselves.
pub struct DatabaseFacts {
    pub server: String,
    pub database: String,
    pub tables: i64,
    pub accounts: i64,
    pub plans: i64,
    pub hosts: i64,
    pub operators: i64,
}

pub async fn database_facts(db: &Db) -> Result<DatabaseFacts> {
    let (server, database): (String, String) =
        sqlx::query_as("SELECT version(), current_database()").fetch_one(db).await?;
    // The major version is the useful part; the rest is build and platform.
    let server = server.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
    let tables: i64 =
        sqlx::query_scalar("SELECT count(*) FROM information_schema.tables WHERE table_schema='public'")
            .fetch_one(db)
            .await?;
    // Counted individually rather than in one query so a table that does not
    // exist yet reports zero instead of failing the whole banner.
    //
    // Each count is its own statement, not a `format!` over a table name: a
    // name spliced into SQL is invisible to anything checking the SQL, and a
    // count that fails here is swallowed as zero — so a wrong name would not
    // error, it would just make the banner lie about an empty database.
    async fn count(db: &Db, sql: &str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(db).await.unwrap_or(0)
    }
    Ok(DatabaseFacts {
        server,
        database,
        tables,
        accounts: count(db, "SELECT count(*) FROM account").await,
        plans: count(db, "SELECT count(*) FROM plan").await,
        hosts: count(db, "SELECT count(*) FROM host").await,
        operators: count(db, "SELECT count(*) FROM admin_user").await,
    })
}

pub async fn upsert_admin_user(db: &Db, email: &str, password_hash: &str) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO admin_user (email,password_hash) VALUES (lower($1),$2)
           ON CONFLICT (email) DO UPDATE SET password_hash=excluded.password_hash"#,
    )
    .bind(email)
    .bind(password_hash)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn get_admin_password_hash(db: &Db, email: &str) -> Result<Option<String>> {
    Ok(sqlx::query_scalar(r#"SELECT password_hash FROM admin_user WHERE email=lower($1)"#)
        .bind(email)
        .fetch_optional(db)
        .await?)
}

// ---- pool dispatch: assignment, claiming, heartbeats -----------------------

/// Admin placement: pins a queued, unplaced run to (host, pod). Guarded so a
/// racing cancel (or double placement) loses cleanly.
pub async fn assign_execution(db: &Db, execution_id: i64, host_id: i64, pod: &str) -> Result<bool> {
    Ok(sqlx::query(
        r#"UPDATE execution SET host_id=$2, pod_name=$3 WHERE execution_id=$1 AND status='queued' AND host_id IS NULL"#,
    )
    .bind(execution_id)
    .bind(host_id)
    .bind(pod)
    .execute(db)
    .await?
    .rows_affected()
        > 0)
}

/// The pod supervisor's poll: claim the oldest run assigned to me. Atomic via
/// FOR UPDATE SKIP LOCKED, so a replaced pod with the same name can't double-claim.
pub async fn claim_next_execution(db: &Db, host_id: i64, pod: &str) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar(
        r#"UPDATE execution SET claimed_at=now(), heartbeat_at=now()
           WHERE execution_id = (
               SELECT execution_id FROM execution
               WHERE status='queued' AND host_id=$1 AND pod_name=$2 AND claimed_at IS NULL
               ORDER BY execution_id LIMIT 1 FOR UPDATE SKIP LOCKED)
           RETURNING execution_id"#,
    )
    .bind(host_id)
    .bind(pod)
    .fetch_optional(db)
    .await?)
}

/// One round-trip: refresh the heartbeat and learn whether a cancel is wanted.
pub async fn heartbeat_execution(db: &Db, execution_id: i64) -> Result<bool> {
    Ok(sqlx::query_scalar(
        r#"UPDATE execution SET heartbeat_at=now() WHERE execution_id=$1 RETURNING cancel_requested"#,
    )
    .bind(execution_id)
    .fetch_optional(db)
    .await?
    .unwrap_or(false))
}

pub async fn request_execution_cancel(db: &Db, execution_id: i64) -> Result<()> {
    sqlx::query(r#"UPDATE execution SET cancel_requested=true WHERE execution_id=$1"#)
        .bind(execution_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Queued runs the placement loop has not yet routed to a pod.
pub async fn unplaced_queued_executions(db: &Db, limit: i64) -> Result<Vec<i64>> {
    Ok(sqlx::query_scalar(
        r#"SELECT execution_id FROM execution WHERE status='queued' AND host_id IS NULL ORDER BY execution_id LIMIT $1"#,
    )
    .bind(limit.clamp(1, 200))
    .fetch_all(db)
    .await?)
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct PoolExecution {
    pub execution_id: i64,
    pub host_id: i64,
    pub pod_name: String,
    pub status: String,
}

/// Every non-terminal placed run — the busy/free map for placement and the UI.
pub async fn pool_execution_snapshot(db: &Db) -> Result<Vec<PoolExecution>> {
    let rows = sqlx::query(
        r#"SELECT execution_id,host_id,pod_name,status FROM execution
           WHERE status IN ('queued','running') AND host_id IS NOT NULL AND pod_name IS NOT NULL"#,
    )
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(PoolExecution::from_row).collect::<Result<Vec<_>, _>>()?)
}

/// Claimed runs whose supervisor stopped heartbeating (pod SIGKILL, node
/// loss): fail them so the plan unblocks. Returns the affected run ids.
pub async fn reap_stale_pool_executions(db: &Db, stale_secs: i64) -> Result<Vec<i64>> {
    Ok(sqlx::query_scalar(
        r#"UPDATE execution SET status='failed', exit_code=-1, finished_at=now()
           WHERE status IN ('queued','running') AND claimed_at IS NOT NULL
             AND heartbeat_at < now() - make_interval(secs => $1)
           RETURNING execution_id"#,
    )
    .bind(stale_secs as f64)
    .fetch_all(db)
    .await?)
}

/// Queued-but-unclaimed runs assigned to a pod that no longer exists on the
/// host (scale-down, dead pod): clear the placement so they are re-routed.
/// Returns the affected run ids so the caller can write the routing log.
pub async fn unassign_lost_executions(db: &Db, host_id: i64, live_pods: &[String]) -> Result<Vec<i64>> {
    Ok(sqlx::query_scalar(
        r#"UPDATE execution SET host_id=NULL, pod_name=NULL
           WHERE status='queued' AND claimed_at IS NULL AND host_id=$1 AND NOT (pod_name = ANY($2))
           RETURNING execution_id"#,
    )
    .bind(host_id)
    .bind(live_pods)
    .fetch_all(db)
    .await?)
}

/// One routing-audit row (see route_log.sql). Best-effort at every call site —
/// a failed audit write must never fail a placement.
pub async fn append_route_log(db: &Db, execution_id: i64, event: &str, host_id: Option<i64>, pod: Option<&str>, detail: &str) -> Result<()> {
    sqlx::query(r#"INSERT INTO route_log (execution_id,event,host_id,pod_name,detail) VALUES ($1,$2,$3,$4,$5)"#)
        .bind(execution_id)
        .bind(event)
        .bind(host_id)
        .bind(pod)
        .bind(&detail.chars().take(200).collect::<String>())
        .execute(db)
        .await?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct RouteLogRow {
    pub route_log_id: i64,
    pub execution_id: i64,
    pub event: String,
    pub host_id: Option<i64>,
    pub host_name: Option<String>,
    pub pod_name: Option<String>,
    pub detail: String,
    pub source: Option<String>,
    pub account_id: Option<i64>,
    pub created_at: DateTime<Utc>,
}

/// The routing audit trail, newest first, joined with plan + host names for
/// display. Admin-only (unscoped).
pub async fn list_route_log(db: &Db, limit: i64) -> Result<Vec<RouteLogRow>> {
    let rows = sqlx::query(
        r#"SELECT l.route_log_id, l.execution_id, l.event, l.host_id, h.name AS host_name, l.pod_name,
                  l.detail, p.source, r.account_id, l.created_at
           FROM route_log l
           LEFT JOIN execution r ON r.execution_id=l.execution_id
           LEFT JOIN plan p ON p.plan_id=r.plan_id
           LEFT JOIN host h ON h.host_id=l.host_id
           ORDER BY l.route_log_id DESC LIMIT $1"#,
    )
    .bind(limit.clamp(1, 1000))
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(RouteLogRow::from_row).collect::<Result<Vec<_>, _>>()?)
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct AdminExecutionRow {
    pub execution_id: i64,
    pub plan_id: i64,
    pub account_id: i64,
    pub source: String,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub host_id: Option<i64>,
    pub pod_name: Option<String>,
}

/// Unscoped, admin-only view of recent runs with their placement.
pub async fn recent_executions_admin(db: &Db, host_id: Option<i64>, pod: Option<&str>, limit: i64) -> Result<Vec<AdminExecutionRow>> {
    let rows = sqlx::query(
        r#"SELECT r.execution_id, r.plan_id, r.account_id, p.source, r.status, r.started_at, r.finished_at,
                  r.host_id, r.pod_name
           FROM execution r JOIN plan p ON p.plan_id=r.plan_id
           WHERE ($1::bigint IS NULL OR r.host_id=$1) AND ($2::varchar IS NULL OR r.pod_name=$2)
           ORDER BY r.execution_id DESC LIMIT $3"#,
    )
    .bind(host_id)
    .bind(pod)
    .bind(limit.clamp(1, 500))
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(AdminExecutionRow::from_row).collect::<Result<Vec<_>, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The schema is what makes the feature off for everyone, so that is what
    /// the test reads. A default that drifts to `true` in a later edit would
    /// silently switch it on for every existing workspace at the next migrate.
    #[test]
    fn connected_logins_default_to_off() {
        let sql = include_str!("../../local-infra/db/public/account.sql");
        let line = sql
            .lines()
            .find(|l| l.contains("connected_logins") && l.contains("ADD COLUMN"))
            .expect("account.sql must define connected_logins");
        assert!(line.contains("DEFAULT false"), "connected logins must default to off, got: {line}");
        assert!(line.contains("NOT NULL"), "a null would read as neither on nor off: {line}");
    }

    #[test]
    fn schedule_normalisation() {
        assert_eq!(normalize_schedule_time("9:05").unwrap(), "09:05");
        assert_eq!(normalize_schedule_time("").unwrap(), "");
        assert!(normalize_schedule_time("25:00").is_err());
        assert_eq!(normalize_schedule_days("1, 3,1").unwrap(), "1,3");
        assert_eq!(normalize_schedule_days("0,1,2,3,4,5,6").unwrap(), "");
        assert!(normalize_schedule_days("7").is_err());
    }

    #[test]
    fn next_fire_respects_days_and_zone() {
        // Wednesday 2026-09-02 12:00 UTC == 07:00 New York.
        let now = Utc.with_ymd_and_hms(2026, 9, 2, 12, 0, 0).unwrap();
        let next = next_fire("09:00", "", "America/New_York", now).unwrap();
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 9, 2, 13, 0, 0).unwrap());
        // Only Mondays (1): the following Monday, 7 Sep.
        let next = next_fire("09:00", "1", "America/New_York", now).unwrap();
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 9, 7, 13, 0, 0).unwrap());
    }

    #[test]
    fn urls_are_redacted() {
        assert_eq!(redact("postgres://u:pw@h:1/db"), "postgres://***@h:1/db");
        assert_eq!(redact("postgres://h/db"), "postgres://h/db");
    }
}

#[cfg(test)]
mod kind_permission_tests {
    use super::*;

    #[test]
    fn a_stored_list_keeps_only_real_kinds() {
        assert_eq!(parse_kinds("artifacts, report"), ["artifacts", "report"]);
        // PlanKind::parse maps anything unknown to prospects; that must not
        // turn a typo into a granted permission.
        assert!(parse_kinds("reprot, nonsense").is_empty());
        assert_eq!(parse_kinds("ARTIFACTS,artifacts"), ["artifacts"], "case folded and deduped");
        assert!(parse_kinds("").is_empty());
    }

    #[test]
    fn the_built_in_default_is_the_two_finished_kinds() {
        assert_eq!(DEFAULT_KINDS, ["prospects", "artifacts"]);
        assert!(!DEFAULT_KINDS.contains(&"report"));
        assert!(!DEFAULT_KINDS.contains(&"assets"));
    }
}
