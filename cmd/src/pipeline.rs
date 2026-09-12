//! The run itself: SCRAPE → DEDUPE → ENRICH → STORE, driven by one `Execution` row.
//!
//! Executed as its own process (`huntwell run --execution-id N`) exactly as the
//! original did, because the browser, the guard and the trail all keep
//! process-wide state that assumes one run per process. The server spawns it,
//! tails its stdout into `ExecutionLog`, and reads the outcome back from `Execution`.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::{ask_agent, AgentOpts};
use crate::browser;
use crate::guard;
use crate::progress::{fmt_elapsed, human_bytes, one_line, Level};
use crate::prospect::{self, render_template, Ctx, Mapping};
use crate::store::{self, Db, SourceConfig};
use crate::trail;

const PROMPT_HASH_KEY: &str = "_prompt_hash";
const PROMPT_PROPOSAL_KEY: &str = "_scrape_prompt";
const MAX_VARIANT_PROMPT_BYTES: usize = 16 * 1024;

/// What the UI (or the scheduler) asked for. Stored on the run as ArgsJson.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunArgs {
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    #[serde(default)]
    pub no_enrich: bool,
    #[serde(default)]
    pub learn: Option<bool>,
    #[serde(default)]
    pub free_agent: Option<bool>,
    #[serde(default)]
    pub iterations: Option<i64>,
    #[serde(default)]
    pub target: Option<i64>,
    #[serde(default)]
    pub restart: bool,
    #[serde(default = "default_progress")]
    pub progress: String,
    #[serde(default)]
    pub model: Option<String>,
}

fn default_progress() -> String {
    "normal".into()
}

struct RuntimeOpts {
    no_enrich: bool,
    sleep: Duration,
    learn: bool,
    free_agent: bool,
    iterations: i64,
    target_prospects: i64,
    min_value: i64,
    frontier_ttl_hours: i64,
    restart: bool,
    agent: AgentOpts,
    progress: Level,
}

fn short_hash(h: &str) -> String {
    h.chars().take(10).collect()
}

fn seed_var_names(seed: &Ctx) -> BTreeSet<String> {
    seed.keys().filter(|k| !k.starts_with('_')).cloned().collect()
}

/// The projection from a raw scraped row to a Prospect.
pub fn mapping_from_config(sc: &SourceConfig) -> Mapping {
    Mapping {
        source: sc.source.clone(),
        source_key: sc.source_key_tmpl.clone(),
        name: sc.name_tmpl.clone(),
        title: sc.title_tmpl.clone(),
        company: sc.company_tmpl.clone(),
        industry: sc.industry_tmpl.clone(),
        email: sc.email_tmpl.clone(),
        email_status: sc.email_status_tmpl.clone(),
        phone: sc.phone_tmpl.clone(),
        website: sc.website_tmpl.clone(),
        linkedin: sc.linkedin_tmpl.clone(),
        location: sc.location_tmpl.clone(),
        notes: sc.notes_tmpl.clone(),
        estimated_value: sc.estimated_value_tmpl.clone(),
    }
}

/// Entry point for `huntwell run --execution-id N`. Returns the process exit code.
pub async fn execution_by_id(db: &Db, execution_id: i64) -> i32 {
    let run = match store::get_execution_unscoped(db, execution_id).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            eprintln!("error: run {execution_id} does not exist");
            return 2;
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            return 2;
        }
    };
    let sc = match store::get_plan_unscoped(db, run.plan_id).await {
        Ok(Some(p)) => p,
        _ => {
            eprintln!("error: plan {} for run {execution_id} does not exist", run.plan_id);
            let _ = store::finish_execution(db, execution_id, "failed", Some(2)).await;
            return 2;
        }
    };
    let args: RunArgs = serde_json::from_value(run.args_json.clone()).unwrap_or_default();

    // Per-account browser, per-run DevTools port. Set here rather than by the
    // server so a run started by hand behaves the same.
    let data = crate::config::data_dir();
    std::env::set_var("HUNTWELL_CHROME_DIR", data.join("accounts").join(run.account_id.to_string()).join("chrome"));
    if let Some(port) = run.cdp_port {
        std::env::set_var("HUNTWELL_CDP_PORT", port.to_string());
    }
    std::env::set_var("HUNTWELL_EXECUTION_ID", execution_id.to_string());
    std::env::set_var("HUNTWELL_SESSION", format!("huntwell-run-{execution_id}"));
    // The account + run this process meters token usage against.
    METER_EXECUTION.store(execution_id, std::sync::atomic::Ordering::Relaxed);
    METER_ACCOUNT.store(run.account_id, std::sync::atomic::Ordering::Relaxed);
    METER_PLAN.store(run.plan_id, std::sync::atomic::Ordering::Relaxed);
    // Write each turn to the row as the agent reports it, so the watching
    // page ticks instead of sitting at $0 until the (minutes-long) call ends.
    let meter_db = db.clone();
    let meter_handle = tokio::runtime::Handle::current();
    let meter_account = run.account_id;
    let meter_plan = run.plan_id;
    reset_real_booked();
    crate::agent::set_usage_flusher(Some(std::sync::Arc::new(move |u, cost_micros| {
        let db = meter_db.clone();
        meter_handle.block_on(async move {
            book_tokens(&db, execution_id, meter_account, meter_plan, u, cost_micros).await;
        });
    })));
    let display_db = db.clone();
    let display_handle = tokio::runtime::Handle::current();
    crate::agent::set_display_flusher(Some(std::sync::Arc::new(move |est| {
        let db = display_db.clone();
        display_handle.block_on(async move {
            show_live_tokens(&db, execution_id, meter_account, meter_plan, est).await;
        });
    })));

    // The admin's model choices, read once and published to this process.
    crate::agent::set_stage_models(store::stage_models(db).await);

    // Whether this account may collect from the restricted platforms. Read
    // from the account rather than the plan: it is a claim about a right the
    // person holds, not a setting on a search.
    let acked = matches!(store::get_account(db, run.account_id).await, Ok(Some(a)) if a.platform_ack_at.is_some());
    crate::guard::set_restricted_allowed(acked);

    let _ = store::set_execution_running(db, execution_id, Some(std::process::id())).await;

    let outcome = execute(db, &run, &sc, &args).await;

    // Whatever happened, the Chrome this run started goes away with it. An
    // adopted one (a sibling run of the same account) is left alone; a
    // Browserbase session is released so it stops billing.
    browser::stop_for_run();
    crate::browserbase::stop().await;
    match outcome {
        Ok(new) => {
            let _ = store::set_execution_new_prospects(db, execution_id, new).await;
            let _ = store::finish_execution(db, execution_id, "succeeded", Some(0)).await;
            // Sent from the run itself, not the server: this is the only place
            // that holds for every dispatch mode, and it is where the count of
            // genuinely new rows is known.
            if new > 0 && sc.alert_email {
                alert_new_records(db, &sc, new).await;
            }
            0
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            let _ = store::finish_execution(db, execution_id, "failed", Some(1)).await;
            1
        }
    }
}

/// Emails the plan's owner that a run found something new.
///
/// Best-effort by design: an alert that cannot be sent must not fail a run that
/// worked. Every failure here is a log line, never an error return. Without
/// `HUNTWELL_PUBLIC_URL` there is nowhere to link to, so nothing is sent —
/// an email whose only call to action is a dead link is worse than silence.
async fn alert_new_records(db: &Db, sc: &SourceConfig, new: i64) {
    let Some(base) = crate::config::get("HUNTWELL_PUBLIC_URL") else {
        return;
    };
    let Ok(Some(acc)) = store::get_account(db, sc.account_id).await else {
        return;
    };
    let kind = sc.kind_of().as_str().to_string();
    let noun = match kind.as_str() {
        "artifacts" => "rows",
        "assets" => "files",
        "report" => "reports",
        _ => "results",
    };
    let total = store::plan_row_count(db, sc.plan_id, &kind).await;
    let samples = store::recent_labels(db, sc.plan_id, &kind, new.min(5)).await;
    let link = format!("{}/app/plans/{}", base.trim_end_matches('/'), sc.plan_id);
    let msg = crate::mail::new_records(&sc.source, noun, new, total, &samples, &link);
    // Queued, not sent: this is the tail end of a run, and a slow mail provider
    // must not hold the process open or lose the alert if it fails.
    if let Err(e) = store::queue_mail(db, Some(sc.account_id), &acc.email, "new_records", &msg).await {
        eprintln!("  ! could not queue the alert email: {e:#}");
    }
}

/// The agent options for one stage of a run.
///
/// Order: a model passed on the command line (or the run args) is a deliberate
/// all-stages override; then the plan's per-stage choice; then the legacy
/// all-stages `model` column; then the install default. A scrape and an
/// enrichment are not the same job and should not be priced as though they were.
fn stage_opts(base: &AgentOpts, stage: &str, sc: &SourceConfig) -> AgentOpts {
    if base.model.is_some() {
        return base.clone();
    }
    let model = sc.stage_model(stage).or_else(|| crate::agent::stage_model(stage));
    AgentOpts { model, ..base.clone() }
}

async fn execute(db: &Db, run: &store::ExecutionRecord, sc: &SourceConfig, args: &RunArgs) -> Result<i64> {
    let mut seed: Ctx = sc.seed_vars();
    for (k, v) in &args.vars {
        seed.insert(k.clone(), Value::String(v.clone()));
    }
    let learn = args.learn.unwrap_or(sc.learn);
    let mut free_agent = args.free_agent.unwrap_or(sc.free_agent);
    if free_agent && (!learn || sc.planner_prompt.trim().is_empty()) {
        println!("[warn] free agent needs learn mode and a planner prompt — running without it");
        free_agent = false;
    }
    // Only a run-level `--model` pins every stage. The plan's per-stage
    // columns are applied in `stage_opts`, so a search model does not leak
    // into enrichment.
    let model = args.model.clone().filter(|m| !m.trim().is_empty());
    let opts = RuntimeOpts {
        no_enrich: args.no_enrich,
        sleep: Duration::from_secs(2),
        learn,
        free_agent,
        iterations: args.iterations.filter(|n| *n >= 1).unwrap_or(sc.iterations as i64).max(1),
        target_prospects: args.target.filter(|n| *n >= 0).unwrap_or(sc.target_prospects as i64),
        min_value: sc.min_value,
        frontier_ttl_hours: 168,
        restart: args.restart,
        agent: AgentOpts {
            force: true,
            progress: Level::parse(&args.progress).unwrap_or(Level::Normal),
            model: model.and_then(|m| crate::agent::normalize_model(&m)),
        },
        progress: Level::parse(&args.progress).unwrap_or(Level::Normal),
    };

    guard::set_nav_allowlist(guard::split_hosts(&sc.allow_hosts));
    crate::sandbox::set_run_scope(&sc.source, sc.plan_id);

    print_run_header(db, run, sc, &seed, &opts).await;

    if crate::config::get("CURSOR_API_KEY").is_none() {
        println!("[warn] CURSOR_API_KEY is not set — the agent will only work if `agent login` was run on this machine");
    }

    println!();
    if crate::browserbase::selected() && !crate::browserbase::configured() {
        anyhow::bail!(
            "HUNTWELL_BROWSER=browserbase but BROWSERBASE_API_KEY / BROWSERBASE_PROJECT_ID are not set.              Set them (in local-infra/global), or unset HUNTWELL_BROWSER to use local Chrome."
        );
    }
    if crate::browserbase::configured() {
        // The account's persistent Context, so the run scrapes logged in.
        let context = crate::store::account_context(db, sc.account_id).await.unwrap_or(None);
        crate::browserbase::start(sc.account_id, context).await.context("start Browserbase session")?;
        // Recorded so the owner can watch this execution while it runs. The id
        // alone: the viewing URL is fetched per request, after an ownership
        // check, and never stored.
        if let Some(sid) = crate::browserbase::session_id() {
            let _ = store::set_execution_browser_session(db, run.execution_id, &sid).await;
        }
    }
    browser::start_for_run().context("start the browser")?;

    // Ctrl-C / SIGTERM (the server's cancel) — tidy the browser and mark the
    // run cancelled ourselves, since the parent may already be gone.
    {
        let db = db.clone();
        let execution_id = run.execution_id;
        let _ = ctrlc::set_handler(move || {
            browser::stop_for_run();
            crate::browserbase::stop_blocking();
            let db = db.clone();
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build();
            if let Ok(rt) = rt {
                let _ = rt.block_on(store::finish_execution(&db, execution_id, "cancelled", Some(130)));
            }
            std::process::exit(130);
        });
    }

    run_pipeline(db, sc, &seed, opts).await
}

async fn print_run_header(db: &Db, run: &store::ExecutionRecord, sc: &SourceConfig, seed: &Ctx, opts: &RuntimeOpts) {
    println!("huntwell run {:?}  (run #{})", sc.source, run.execution_id);
    println!(
        "  audience    {}",
        if sc.targets_individuals() {
            "individual — a private person's own contact details"
        } else {
            "business — company contact details (work address, work email)"
        }
    );
    if opts.learn {
        println!(
            "  mode        learn — up to {} iterations, stop after {} zero-yield",
            opts.iterations, sc.max_no_progress
        );
    } else {
        println!("  mode        single pass");
    }
    if opts.target_prospects > 0 {
        println!(
            "  target      {} new unique {} (run ends when reached)",
            opts.target_prospects,
            result_noun(sc, opts.target_prospects)
        );
    }
    if opts.free_agent {
        println!("  free agent  on — planner may author replacement scrape prompts");
    }
    println!("  model       {}", opts.agent.model.as_deref().unwrap_or("auto"));
    let vars: Vec<String> = seed.iter().map(|(k, v)| format!("{k}={}", value_str(v))).collect();
    println!("  seed vars   {}", if vars.is_empty() { "(none)".into() } else { vars.join("  ") });
    if opts.no_enrich || sc.enrich_prompt.trim().is_empty() {
        println!("  enrich      off");
    } else {
        println!("  enrich      on, {} between calls", fmt_elapsed(opts.sleep));
    }
    if crate::browserbase::selected() {
        println!("  browser     Browserbase (remote cloud browser){}", if crate::browserbase::configured() { "" } else { " — MISSING KEYS, run will fail" });
    } else {
        println!("  browser     Chrome on DevTools port {}, up to {}s to start", browser::cdp_port(), browser::wait_secs());
    }
    if opts.min_value > 0 {
        println!("  min value   {}", human_money(opts.min_value));
    }
    let already = store::plan_row_count(db, sc.plan_id, sc.kind_of().as_str()).await;
    println!(
        "  already     {} {} stored under this plan",
        already,
        result_noun(sc, already)
    );
    if let Ok((q, p)) = store::search_trail_counts(db, sc.plan_id).await {
        if q + p > 0 {
            println!("  trail       {q} search(es) and {p} page(s) on record — this run starts somewhere else");
        }
    }
    if !opts.restart {
        if let Ok(n) = store::pending_seed_count(db, sc.plan_id).await {
            if n > 0 {
                println!("  resume      {n} seed(s) queued by an earlier run, drained first");
            }
        }
    }
}

fn value_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

pub fn human_money(v: i64) -> String {
    let f = v as f64;
    if f >= 1e9 {
        format!("${:.1}B", f / 1e9)
    } else if f >= 1e6 {
        format!("${:.1}M", f / 1e6)
    } else if f >= 1e3 {
        format!("${:.0}K", f / 1e3)
    } else {
        format!("${v}")
    }
}

/// Runs the agent off the async runtime — each call is a blocking child
/// process wait that can last minutes.
async fn agent_call(db: &Db, label: &str, prompt: String, opts: AgentOpts) -> Result<Value> {
    let label = label.to_string();
    let called = label.clone();
    let progress = opts.progress;
    let joined = tokio::task::spawn_blocking(move || ask_agent(&called, &prompt, opts)).await;
    // Meter the tokens this call spent — whether it succeeded or not — so the
    // account's usage ticks up live and the budget cap sees it on the next run.
    meter_run(db, &label, progress).await;
    joined.map_err(|e| anyhow!("agent task panicked: {e}"))?
}

/// The run + account this process is executing, for [`meter_run`]. One run per
/// process, so process globals are safe; set in [`execution_by_id`].
static METER_EXECUTION: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
static METER_ACCOUNT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
static METER_PLAN: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// Billed tokens already folded into the account. The live page shows this
/// plus the current call's estimate; a new estimate *replaces* the last one
/// rather than stacking on the run row.
fn real_booked() -> &'static std::sync::Mutex<store::TokenUsage> {
    static B: std::sync::OnceLock<std::sync::Mutex<store::TokenUsage>> = std::sync::OnceLock::new();
    B.get_or_init(|| std::sync::Mutex::new(store::TokenUsage::default()))
}

fn reset_real_booked() {
    if let Ok(mut g) = real_booked().lock() {
        *g = store::TokenUsage::default();
    }
}

fn add_real_booked(u: store::TokenUsage) -> store::TokenUsage {
    real_booked()
        .lock()
        .map(|mut g| {
            g.add(u);
            *g
        })
        .unwrap_or(u)
}

fn real_booked_now() -> store::TokenUsage {
    real_booked().lock().map(|g| *g).unwrap_or_default()
}

/// The run this process is executing, for results that record their origin.
/// `None` outside a run (a draft, a test).
fn current_execution_id() -> Option<i64> {
    match METER_EXECUTION.load(std::sync::atomic::Ordering::Relaxed) {
        0 => None,
        id => Some(id),
    }
}

async fn meter_run(db: &Db, label: &str, progress: Level) {
    // Leftover from the last events (or the whole call, when no live flusher
    // is installed). Already-flushed turns are on the row; this writes only
    // what has not been booked yet.
    let leftover = crate::agent::take_usage();
    use std::sync::atomic::Ordering;
    let execution_id = METER_EXECUTION.load(Ordering::Relaxed);
    let account_id = METER_ACCOUNT.load(Ordering::Relaxed);
    if execution_id > 0 && account_id > 0 && !(leftover.0.is_zero() && leftover.1 == 0) {
        let plan_id = METER_PLAN.load(Ordering::Relaxed);
        book_tokens(db, execution_id, account_id, plan_id, leftover.0, leftover.1).await;
    }
    let (u, cost_micros) = crate::agent::take_call_total();
    if u.is_zero() && cost_micros == 0 {
        return;
    }
    // Which stage spent it, so the end of the run can say where the tokens
    // went rather than only how many there were.
    crate::meter::record(label, u, cost_micros);
    if progress != Level::Off {
        println!("  tokens     {}", crate::meter::call_line(u, cost_micros));
    }
}

/// Books Cursor's reported usage: the account is charged, and the run row
/// is set to the billed total (replacing any estimate that was showing).
async fn book_tokens(
    db: &Db,
    execution_id: i64,
    account_id: i64,
    plan_id: i64,
    u: store::TokenUsage,
    cost_micros: i64,
) {
    if u.is_zero() && cost_micros == 0 {
        return;
    }
    let total = add_real_booked(u);
    if let Err(e) = store::set_execution_token_totals(db, execution_id, total).await {
        tracing::warn!("meter run tokens: {e:#}");
        return;
    }
    if let Err(e) = store::add_account_usage(db, account_id, u, cost_micros).await {
        tracing::warn!("meter account tokens: {e:#}");
    }
    publish_meter(account_id, execution_id, plan_id, total.billable()).await;
}

/// Writes the climbing estimate to the run row. Not a charge.
async fn show_live_tokens(
    db: &Db,
    execution_id: i64,
    account_id: i64,
    plan_id: i64,
    est: store::TokenUsage,
) {
    if est.is_zero() {
        return;
    }
    let mut shown = real_booked_now();
    shown.add(est);
    if let Err(e) = store::set_execution_token_totals(db, execution_id, shown).await {
        tracing::warn!("meter live display: {e:#}");
        return;
    }
    publish_meter(account_id, execution_id, plan_id, shown.billable()).await;
}

async fn publish_meter(account_id: i64, execution_id: i64, plan_id: i64, tokens: i64) {
    crate::bus::publish(
        crate::bus::subject::RUN_METERED,
        Some(account_id),
        serde_json::json!({
            "execution_id": execution_id,
            "plan_id": plan_id,
            "tokens": tokens,
            "cost_usd": tokens as f64 * crate::config::sell_usd_per_mtoken() / 1e6,
        }),
    )
    .await;
}

async fn run_pipeline(db: &Db, sc: &SourceConfig, initial_seed: &Ctx, opts: RuntimeOpts) -> Result<i64> {
    let mapping = mapping_from_config(sc);
    // Custom-artifact plans project rows through a schema instead of the prospect
    // templates (see iterate_artifacts).
    let amap = crate::artifact::ArtifactMapping::new(crate::artifact::parse_schema(&sc.fields_schema_json));
    // One definition of "can this run", shared with the API and the runner.
    if let Err(why) = store::plan_ready(sc) {
        anyhow::bail!("{why}");
    }
    let kind = sc.kind_of();
    // A report is one document, not an accumulation: the agent does all its
    // searching inside a single call, so extra iterations would only rewrite
    // the same page. The row kinds keep the learn-mode loop.
    let iterations = match kind {
        store::PlanKind::Report => 1,
        _ if opts.learn => opts.iterations,
        _ => 1,
    };
    let pipeline_started = Instant::now();

    let mut queue: Vec<Ctx> = if opts.restart {
        let dropped = store::clear_pending_seeds(db, sc.plan_id).await.unwrap_or(0);
        if dropped > 0 {
            println!("[resume] restart: dropped {dropped} queued seed(s)");
        }
        vec![initial_seed.clone()]
    } else {
        let resumed = resume_queue(db, sc.plan_id, opts.iterations).await;
        if resumed.is_empty() {
            vec![initial_seed.clone()]
        } else {
            println!("[resume] continuing from {} seed(s) queued by an earlier run", resumed.len());
            resumed
        }
    };
    let mut no_progress = 0i64;
    let mut total_new = 0i64;
    let initial_seed_key = crate::sha1_hex(&serde_json::to_string(initial_seed)?);

    let mut iter = 0i64;
    while iter < iterations && !queue.is_empty() {
        let seed = queue.remove(0);
        let seed_json = serde_json::to_string(&seed)?;
        let seed_key = crate::sha1_hex(&seed_json);

        let is_initial = seed_key == initial_seed_key;
        if opts.learn && !is_initial && store::was_explored(db, sc.plan_id, &seed_key, opts.frontier_ttl_hours).await? {
            println!(
                "\n[iteration {}/{iterations}] skip — seed already explored within {}h: {seed_json}",
                iter + 1,
                opts.frontier_ttl_hours
            );
            iter += 1;
            continue;
        }

        println!("\n═══ iteration {}/{} ══════════════════════════════════════════", iter + 1, iterations);
        println!("seed        {seed_json}");
        if opts.target_prospects > 0 {
            println!(
                "queued      {} more seed(s) waiting · {total_new}/{} new {} so far",
                queue.len(),
                opts.target_prospects,
                result_noun(sc, opts.target_prospects)
            );
        } else {
            println!(
                "queued      {} more seed(s) waiting · {total_new} new {} so far",
                queue.len(),
                result_noun(sc, total_new)
            );
        }

        let remaining = if opts.target_prospects > 0 {
            let left = opts.target_prospects - total_new;
            if left <= 0 {
                println!(
                    "[stop] target already met — {total_new}/{} new unique {}",
                    opts.target_prospects,
                    result_noun(sc, opts.target_prospects)
                );
                break;
            }
            Some(left)
        } else {
            None
        };

        let started = Instant::now();
        let iter_result = match kind {
            store::PlanKind::Artifacts => iterate_artifacts(db, sc, &amap, &seed, &opts, remaining, iter).await,
            store::PlanKind::Report => iterate_report(db, sc, &seed, &opts, iter).await,
            store::PlanKind::Assets => iterate_assets(db, sc, &seed, &opts, remaining, iter).await,
            store::PlanKind::Prospects => iterate_once(db, sc, &mapping, &seed, &opts, remaining, iter).await,
        };
        let new_count = match iter_result {
            Ok(n) => n,
            Err(e) if e.downcast_ref::<guard::ContainmentBreach>().is_some() => {
                return Err(e.context(format!(
                    "run halted at iteration {} — the agent broke out of what a scrape may do",
                    iter + 1
                )));
            }
            Err(e) if e.downcast_ref::<browser::Unavailable>().is_some() => {
                return Err(e.context(format!("run halted at iteration {} — no browser to scrape with", iter + 1)));
            }
            Err(e) => {
                println!("[iter {}] error: {e:#}", iter + 1);
                0
            }
        };
        let _ = store::mark_explored(db, sc.plan_id, &seed_key, &seed_json, iter, new_count).await;
        let hash = match seed.get(PROMPT_HASH_KEY).and_then(Value::as_str) {
            Some(h) => h.to_string(),
            None => {
                let h = crate::sha1_hex(sc.scrape_prompt.trim());
                let _ = store::save_prompt_variant(db, sc.plan_id, &h, sc.scrape_prompt.trim(), "config").await;
                h
            }
        };
        let _ = store::record_variant_result(db, sc.plan_id, &hash, new_count).await;
        total_new += new_count;
        println!("[iteration {} done] +{new_count} new in {}", iter + 1, fmt_elapsed(started.elapsed()));

        if opts.target_prospects > 0 && total_new >= opts.target_prospects {
            println!(
                "[stop] target reached — {total_new}/{} new unique {}",
                opts.target_prospects,
                result_noun(sc, opts.target_prospects)
            );
            iter += 1;
            break;
        }

        if new_count == 0 {
            no_progress += 1;
            if no_progress >= sc.max_no_progress as i64 {
                println!("[stop] {no_progress} consecutive zero-yield iterations");
                break;
            }
            println!("[warn] zero-yield iteration {no_progress} of {} allowed before stopping", sc.max_no_progress);
        } else {
            no_progress = 0;
        }

        // A planner call is a full agent call. Seeds already queued are seeds
        // this run will reach before it runs out of iterations, so planning on
        // top of them buys nothing and is paid for every iteration.
        let iters_left = iterations - (iter + 1);
        let planner_on = opts.learn && !sc.planner_prompt.trim().is_empty() && iters_left > 0;
        if planner_on && queue.len() as i64 >= iters_left {
            println!(
                "[planner] skipped — {} queued seed(s) already cover the {iters_left} iteration(s) left",
                queue.len()
            );
        } else if planner_on {
            println!("[planner] asking agent where to search next…");
            let seeds = match plan_next_seeds(db, sc, initial_seed, &opts).await {
                Ok(s) => s,
                Err(e) => {
                    println!("[planner] error: {e:#}");
                    Vec::new()
                }
            };
            println!("[planner] suggested {} new seed(s)", seeds.len());
            for s in seeds {
                let mut merged = initial_seed.clone();
                for (k, v) in s {
                    merged.insert(k, v);
                }
                if let Some(proposed) = merged.remove(PROMPT_PROPOSAL_KEY) {
                    match register_variant(db, sc.plan_id, proposed.as_str().unwrap_or("")).await {
                        Ok(hash) => {
                            println!("            + new scrape prompt, variant {}", short_hash(&hash));
                            merged.insert(PROMPT_HASH_KEY.into(), Value::String(hash));
                        }
                        Err(e) => println!("            ! rejected prompt: {e}"),
                    }
                }
                if let Ok(j) = serde_json::to_string(&merged) {
                    let _ = store::queue_seed(db, sc.plan_id, &crate::sha1_hex(&j), &j).await;
                    if opts.progress != Level::Off {
                        println!("            + {}", one_line(&j, 120));
                    }
                }
                queue.push(merged);
            }
        }
        iter += 1;
    }

    if opts.target_prospects > 0 {
        println!(
            "\n[done] {total_new}/{} new {} across {iter} iteration(s) in {}",
            opts.target_prospects,
            result_noun(sc, opts.target_prospects),
            fmt_elapsed(pipeline_started.elapsed())
        );
    } else {
        println!(
            "\n[done] {total_new} total new {} across {iter} iteration(s) in {}",
            result_noun(sc, total_new),
            fmt_elapsed(pipeline_started.elapsed())
        );
    }
    for line in crate::meter::summary(total_new) {
        println!("{line}");
    }
    Ok(total_new)
}

fn known_noun(sc: &SourceConfig) -> &'static str {
    if sc.targets_individuals() {
        "known people"
    } else {
        "known companies"
    }
}

/// What a run collects, as the log should say it. Never "prospects" — that
/// word is leftover machinery. People and custom-schema plans both produce
/// artifacts; reports and files keep their own names.
fn result_noun(sc: &SourceConfig, n: i64) -> &'static str {
    match sc.kind_of() {
        store::PlanKind::Assets => {
            if n == 1 {
                "file"
            } else {
                "files"
            }
        }
        store::PlanKind::Report => {
            if n == 1 {
                "report"
            } else {
                "reports"
            }
        }
        _ => {
            if n == 1 {
                "artifact"
            } else {
                "artifacts"
            }
        }
    }
}

fn insert_known_entities(ctx: &mut Ctx, known: Vec<String>) {
    let csv = Value::String(known.join(", "));
    let arr = Value::Array(known.into_iter().map(Value::String).collect());
    ctx.insert("known_companies_csv".into(), csv.clone());
    ctx.insert("known_people_csv".into(), csv);
    ctx.insert("known_companies".into(), arr.clone());
    ctx.insert("known_people".into(), arr);
}

async fn iterate_once(
    db: &Db,
    sc: &SourceConfig,
    mapping: &Mapping,
    seed: &Ctx,
    opts: &RuntimeOpts,
    max_new: Option<i64>,
    iteration: i64,
) -> Result<i64> {
    let mut scrape_ctx = seed.clone();
    scrape_ctx.insert("plan_type".into(), Value::String(sc.audience()));
    let mut known_count = 0usize;
    if opts.learn {
        if let Ok(known) = store::recent_entities(db, sc.plan_id, sc.known_limit, &sc.audience()).await {
            let known = safe_replay(known, known_noun(sc));
            known_count = known.len();
            insert_known_entities(&mut scrape_ctx, known);
        }
    }
    if let Some(n) = max_new {
        scrape_ctx.insert("target_remaining".into(), Value::from(n));
    }

    let (prompt_source, prompt_label) = match seed.get(PROMPT_HASH_KEY).and_then(Value::as_str) {
        Some(hash) => match store::load_prompt_variant(db, sc.plan_id, hash).await? {
            Some(v) => {
                let contract = prospect::schema_contract(mapping, &seed_var_names(seed));
                (format!("{}{}", v.prompt, contract), format!("variant {} ({})", short_hash(hash), v.origin))
            }
            None => {
                println!("  ! prompt variant {} missing — using stored prompt", short_hash(hash));
                (sc.scrape_prompt.clone(), "stored prompt".to_string())
            }
        },
        None => (sc.scrape_prompt.clone(), "stored prompt".to_string()),
    };

    let rotation = plan_search_rotation(db, sc, iteration).await;
    let rotation_block = rotation.render();
    scrape_ctx.insert("search_rotation".into(), Value::String(rotation_block.clone()));

    let mut prompt = render_template(&prompt_source, &scrape_ctx).context("render scrape prompt")?;
    if !prompt_source.contains("search_rotation") {
        prompt.push_str(&rotation_block);
    }
    prompt.push_str(&sites_block(sc));

    println!(
        "\n[1/4 scrape] {prompt_label}, prompt {}, {known_count} {} excluded",
        human_bytes(prompt.len()),
        known_noun(sc)
    );
    println!("  rotation   {}", rotation.summary());
    if opts.progress.is_verbose() {
        for line in prompt.lines() {
            println!("  | {line}");
        }
    }
    let t = Instant::now();
    let scraped = agent_call(db, "scrape", prompt, stage_opts(&opts.agent, "scrape", sc)).await;
    // Read before anything else spends: this is the scrape's own cost.
    let scrape_tokens = crate::meter::last_call_tokens();
    let used_queries = record_search_trail(db, sc.plan_id, &seed_key_of(seed), opts).await;
    let v = scraped.context("scrape")?;
    let raw_rows = v.as_array().ok_or_else(|| anyhow!("scrape: expected JSON array, got {}", json_type(&v)))?;
    let rows: Vec<Ctx> = raw_rows
        .iter()
        .filter_map(|r| r.as_object())
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .collect();
    println!("  → {} rows returned in {}", rows.len(), fmt_elapsed(t.elapsed()));

    println!("[2/4 dedupe] checking {} rows against stored keys", rows.len());
    let known_keys = store::known_source_keys(db, sc.plan_id).await?;
    struct Pending {
        row: Ctx,
        p: prospect::Prospect,
    }
    let mut queue: Vec<Pending> = Vec::new();
    let enrich_on = !opts.no_enrich && !sc.enrich_prompt.trim().is_empty();
    let (mut dup, mut unmappable, mut below_min, mut no_name) = (0usize, 0usize, 0usize, 0usize);
    for r in &rows {
        let ctx = merge_ctx(seed, r);
        let p = match mapping.map(r, &ctx) {
            Ok(p) => p,
            Err(e) => {
                unmappable += 1;
                println!("     ✗ unmappable: {}", one_line(&e.to_string(), 100));
                continue;
            }
        };
        if known_keys.contains(&p.source_key) {
            dup += 1;
            if opts.progress.is_verbose() {
                println!("     · already stored: {}", p.source_key);
            }
            continue;
        }
        if opts.min_value > 0 && p.estimated_value.map_or(true, |ev| ev < opts.min_value) {
            below_min += 1;
            continue;
        }
        if !enrich_on && p.name.trim().is_empty() {
            no_name += 1;
            println!("     · skip (no name): {} | {}", p.source_key, one_line(&p.company, 50));
            continue;
        }
        if opts.progress != Level::Off {
            println!("     + new: {} | {}", p.source_key, one_line(&p.company, 50));
        }
        queue.push(Pending { row: r.clone(), p });
    }
    println!(
        "  → {} new · {dup} already stored · {unmappable} unmappable · {below_min} below min value · {no_name} no name",
        queue.len()
    );
    crate::meter::note_rows(rows.len() as i64, dup as i64, (unmappable + below_min + no_name) as i64, 0);

    if let Some(max) = max_new {
        if max <= 0 {
            println!("  → target remaining is 0 — storing nothing this iteration");
            return Ok(0);
        }
        if (queue.len() as i64) > max {
            let dropped = queue.len() as i64 - max;
            queue.truncate(max as usize);
            println!(
                "  → capping to {max} new {} for target (dropping {dropped} extra from this scrape)",
                result_noun(sc, max)
            );
        }
    }

    let total = queue.len();
    if total == 0 {
        println!("[3/4 enrich] nothing to do");
        println!("[4/4 store]  nothing to store");
        return Ok(0);
    }
    if enrich_on {
        println!("[3/4 enrich] {total} rows, {} between calls", fmt_elapsed(opts.sleep));
    } else {
        println!("[3/4 enrich] skipped");
    }

    let mut stored = 0i64;
    let mut skipped_no_name = 0i64;
    for (i, item) in queue.iter_mut().enumerate() {
        if enrich_on {
            let company = one_line(&item.p.company, 50);
            println!("  [{}/{}] {company}", i + 1, total);
            let t = Instant::now();
            match enrich_row(
                db,
                &format!("enrich {}/{}", i + 1, total),
                &sc.enrich_prompt,
                seed,
                &mut item.row,
                stage_opts(&opts.agent, "enrich", sc),
                false,
            )
            .await
            {
                Ok(added) => println!("        enriched {added} field(s) in {}", fmt_elapsed(t.elapsed())),
                Err(e) => println!("        enrichment_error: {e:#}"),
            }
            let ctx = merge_ctx(seed, &item.row);
            if let Ok(mut p) = mapping.map(&item.row, &ctx) {
                p.source_key = item.p.source_key.clone();
                item.p = p;
            }
            tokio::time::sleep(opts.sleep).await;
        }
        if item.p.name.trim().is_empty() {
            skipped_no_name += 1;
            println!("  · skip store (no name after enrich): {} | {}", item.p.source_key, one_line(&item.p.company, 50));
            continue;
        }
        store::upsert_prospect(db, sc.account_id, sc.plan_id, &item.p).await.context("upsert")?;
        // Which angles were in play when this row appeared. Approximate the
        // same way the yield count is — every query this iteration used gets
        // the edge — and the graph says so.
        for q in &used_queries {
            let _ = store::record_edge(db, sc.plan_id, current_execution_id(), "query", q, "result", &item.p.source_key).await;
        }
        stored += 1;
        println!(
            "  ✓ stored {}/{}  {} | {} | {}",
            stored,
            total,
            one_line(&item.p.name, 40),
            item.p.source_key,
            one_line(&item.p.company, 50)
        );
    }
    if skipped_no_name > 0 {
        println!("[4/4 store]  {stored} rows written · {skipped_no_name} skipped (no name)");
    } else {
        println!("[4/4 store]  {stored} rows written");
    }
    crate::meter::note_rows(0, 0, skipped_no_name, stored);
    let _ = store::attribute_query_yield(db, sc.plan_id, &used_queries, stored, scrape_tokens).await;
    Ok(stored)
}

/// The artifact counterpart of [`iterate_once`]: SCRAPE→DEDUPE→ENRICH→STORE for
/// a custom-schema plan. Leaner than the prospect path — no known-entity
/// exclusion and no min-value / no-name gates; dedupe is purely on the schema's
/// key field, and rows are stored as `Artifact`s.
async fn iterate_artifacts(
    db: &Db,
    sc: &SourceConfig,
    amap: &crate::artifact::ArtifactMapping,
    seed: &Ctx,
    opts: &RuntimeOpts,
    max_new: Option<i64>,
    iteration: i64,
) -> Result<i64> {
    let mut scrape_ctx = seed.clone();
    if let Some(n) = max_new {
        scrape_ctx.insert("target_remaining".into(), Value::from(n));
    }
    let contract = crate::artifact::schema_contract(amap.schema());
    let (prompt_source, prompt_label) = match seed.get(PROMPT_HASH_KEY).and_then(Value::as_str) {
        Some(hash) => match store::load_prompt_variant(db, sc.plan_id, hash).await? {
            Some(v) => (format!("{}{}", v.prompt, contract), format!("variant {} ({})", short_hash(hash), v.origin)),
            None => (sc.scrape_prompt.clone(), "stored prompt".to_string()),
        },
        None => (sc.scrape_prompt.clone(), "stored prompt".to_string()),
    };
    let rotation = plan_search_rotation(db, sc, iteration).await;
    let rotation_block = rotation.render();
    scrape_ctx.insert("search_rotation".into(), Value::String(rotation_block.clone()));
    let mut prompt = render_template(&prompt_source, &scrape_ctx).context("render scrape prompt")?;
    if !prompt_source.contains("search_rotation") {
        prompt.push_str(&rotation_block);
    }
    prompt.push_str(&sites_block(sc));
    println!("\n[1/4 scrape] {prompt_label}, prompt {}", human_bytes(prompt.len()));
    println!("  rotation   {}", rotation.summary());
    if opts.progress.is_verbose() {
        for line in prompt.lines() {
            println!("  | {line}");
        }
    }
    let t = Instant::now();
    let scraped = agent_call(db, "scrape", prompt, stage_opts(&opts.agent, "scrape", sc)).await;
    // Read before anything else spends: this is the scrape's own cost.
    let scrape_tokens = crate::meter::last_call_tokens();
    let used_queries = record_search_trail(db, sc.plan_id, &seed_key_of(seed), opts).await;
    let v = scraped.context("scrape")?;
    let raw_rows = v.as_array().ok_or_else(|| anyhow!("scrape: expected JSON array, got {}", json_type(&v)))?;
    let rows: Vec<Ctx> = raw_rows
        .iter()
        .filter_map(|r| r.as_object())
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .collect();
    println!("  → {} rows returned in {}", rows.len(), fmt_elapsed(t.elapsed()));

    println!("[2/4 dedupe] checking {} artifacts against stored keys", rows.len());
    let known_keys = store::known_artifact_keys(db, sc.plan_id).await?;
    struct PendingA {
        row: Ctx,
        a: crate::artifact::Artifact,
    }
    let mut queue: Vec<PendingA> = Vec::new();
    let enrich_on = !opts.no_enrich && !sc.enrich_prompt.trim().is_empty();
    let (mut dup, mut unmappable) = (0usize, 0usize);
    for r in &rows {
        let a = match amap.map(r) {
            Ok(a) => a,
            Err(e) => {
                unmappable += 1;
                println!("     ✗ unmappable: {}", one_line(&e.to_string(), 100));
                continue;
            }
        };
        if known_keys.contains(&a.source_key) {
            dup += 1;
            continue;
        }
        if opts.progress != Level::Off {
            println!("     + new: {} | {}", a.source_key, one_line(&a.title, 60));
        }
        queue.push(PendingA { row: r.clone(), a });
    }
    println!("  → {} new · {dup} already stored · {unmappable} unmappable", queue.len());
    crate::meter::note_rows(rows.len() as i64, dup as i64, unmappable as i64, 0);

    if let Some(max) = max_new {
        if max <= 0 {
            println!("  → target remaining is 0 — storing nothing this iteration");
            return Ok(0);
        }
        if (queue.len() as i64) > max {
            let dropped = queue.len() as i64 - max;
            queue.truncate(max as usize);
            println!("  → capping to {max} new artifact(s) (dropping {dropped})");
        }
    }
    let total = queue.len();
    if total == 0 {
        println!("[3/4 enrich] nothing to do");
        println!("[4/4 store]  nothing to store");
        return Ok(0);
    }
    if enrich_on {
        println!("[3/4 enrich] {total} rows, {} between calls", fmt_elapsed(opts.sleep));
    } else {
        println!("[3/4 enrich] skipped");
    }

    let mut stored = 0i64;
    for (i, item) in queue.iter_mut().enumerate() {
        if enrich_on {
            println!("  [{}/{}] {}", i + 1, total, one_line(&item.a.title, 50));
            let t = Instant::now();
            let enrich_prompt = format!("{}{}", sc.enrich_prompt, crate::artifact::enrich_identity_note(amap, &item.a));
            match enrich_row(
                db,
                &format!("enrich {}/{}", i + 1, total),
                &enrich_prompt,
                seed,
                &mut item.row,
                stage_opts(&opts.agent, "enrich", sc),
                true,
            )
            .await
            {
                Ok(added) => println!("        enriched {added} field(s) in {}", fmt_elapsed(t.elapsed())),
                Err(e) => println!("        enrichment_error: {e:#}"),
            }
            // The scrape already named this row. Enrich filling another
            // listing's VIN into the key field used to overwrite every car
            // onto one upsert.
            let wanted = item.row.get(amap.key_field()).map(value_str).unwrap_or_default();
            if !wanted.is_empty() && wanted != item.a.source_key {
                println!("        kept scrape key {} (enrich wanted {wanted})", item.a.source_key);
            }
            amap.lock_key(&mut item.row, &item.a.source_key);
            if let Ok(a) = amap.map(&item.row) {
                item.a = a;
            }
            tokio::time::sleep(opts.sleep).await;
        }
        store::upsert_artifact(db, sc.account_id, sc.plan_id, &item.a).await.context("upsert artifact")?;
        for q in &used_queries {
            let _ = store::record_edge(db, sc.plan_id, current_execution_id(), "query", q, "result", &item.a.source_key).await;
        }
        stored += 1;
        println!("  ✓ stored {}/{}  {} | {}", stored, total, one_line(&item.a.title, 50), item.a.source_key);
    }
    println!("[4/4 store]  {stored} rows written");
    crate::meter::note_rows(0, 0, 0, stored);
    let _ = store::attribute_query_yield(db, sc.plan_id, &used_queries, stored, scrape_tokens).await;
    Ok(stored)
}

/// The contract a report plan's scrape prompt is held to. Appended the same
/// way the row kinds append their schema contract.
const REPORT_CONTRACT: &str = r#"

Respond with ONLY a fenced ```json``` object (no prose outside it), shaped:
{
  "subject": "what the report is about",
  "title": "a headline for the document",
  "markdown": "the full report body in Markdown",
  "sources": [{"title": "page title", "url": "https://…"}]
}
Write the body as Markdown with `##` section headings. Cite claims inline as
[1], [2] matching the order of "sources". Report only what you actually found
on the pages you read — say a thing is unknown rather than inventing it."#;

/// SCRAPE→SYNTHESIZE→STORE for a report plan: one research pass producing one
/// document. Returns 1 when a report was written (the pipeline counts rows, and
/// a report is one), 0 when the agent came back empty.
async fn iterate_report(db: &Db, sc: &SourceConfig, seed: &Ctx, opts: &RuntimeOpts, iteration: i64) -> Result<i64> {
    let mut scrape_ctx = seed.clone();
    scrape_ctx.insert("subject".into(), Value::String(sc.subject.clone()));
    let (prompt_source, prompt_label) = match seed.get(PROMPT_HASH_KEY).and_then(Value::as_str) {
        Some(hash) => match store::load_prompt_variant(db, sc.plan_id, hash).await? {
            Some(v) => (v.prompt.clone(), format!("variant {} ({})", short_hash(hash), v.origin)),
            None => (sc.scrape_prompt.clone(), "stored prompt".to_string()),
        },
        None => (sc.scrape_prompt.clone(), "stored prompt".to_string()),
    };
    let rotation = plan_search_rotation(db, sc, iteration).await;
    let rotation_block = rotation.render();
    scrape_ctx.insert("search_rotation".into(), Value::String(rotation_block.clone()));
    let mut prompt = render_template(&prompt_source, &scrape_ctx).context("render research prompt")?;
    if !prompt_source.contains("search_rotation") {
        prompt.push_str(&rotation_block);
    }
    prompt.push_str(&sites_block(sc));
    prompt.push_str(REPORT_CONTRACT);

    println!("\n[1/3 research] {prompt_label}, prompt {}", human_bytes(prompt.len()));
    println!("  subject    {}", one_line(&sc.subject, 80));
    println!("  rotation   {}", rotation.summary());
    let t = Instant::now();
    let researched = agent_call(db, "research", prompt, stage_opts(&opts.agent, "scrape", sc)).await;
    // Read before anything else spends: this is the scrape's own cost.
    let scrape_tokens = crate::meter::last_call_tokens();
    let used_queries = record_search_trail(db, sc.plan_id, &seed_key_of(seed), opts).await;
    let v = researched.context("research")?;

    println!("[2/3 compose] reading the agent's document");
    // Unlike the row kinds this is ONE object, not an array. Accept a
    // single-element array too — models reach for arrays out of habit.
    let obj = match v.as_object() {
        Some(o) => o.clone(),
        None => v
            .as_array()
            .and_then(|a| a.first())
            .and_then(|f| f.as_object())
            .cloned()
            .ok_or_else(|| anyhow!("research: expected a JSON object, got {}", json_type(&v)))?,
    };
    let field = |k: &str| obj.get(k).and_then(Value::as_str).unwrap_or_default().trim().to_string();
    let markdown = crate::normalize::cleanse_multiline(&field("markdown"));
    if markdown.is_empty() {
        println!("  → the agent returned no document body");
        return Ok(0);
    }
    let subject = {
        let s = crate::normalize::cleanse(&field("subject"));
        if s.is_empty() { sc.subject.trim().to_string() } else { s }
    };
    let title = {
        let s = crate::normalize::cleanse(&field("title"));
        if s.is_empty() { subject.clone() } else { s }
    };
    let sources = obj.get("sources").cloned().unwrap_or(Value::Array(vec![]));
    let source_count = sources.as_array().map(|a| a.len()).unwrap_or(0);
    let words = markdown.split_whitespace().count();
    println!("  → \"{}\" · {words} words · {source_count} source(s) in {}", one_line(&title, 60), fmt_elapsed(t.elapsed()));

    println!("[3/3 store]  writing the report");
    // The subject is the dedupe key, so re-running refreshes in place.
    let source_key = crate::normalize::cleanse_key(&subject);
    let source_key = if source_key.is_empty() { format!("plan-{}", sc.plan_id) } else { source_key };
    store::upsert_report(
        db,
        sc.account_id,
        sc.plan_id,
        current_execution_id(),
        &store::NewReport { source_key, subject, title, markdown, sources },
    )
    .await
    .context("upsert report")?;
    println!("  ✓ report stored");
    let _ = store::attribute_query_yield(db, sc.plan_id, &used_queries, 1, scrape_tokens).await;
    Ok(1)
}

/// SCRAPE→FILTER→DOWNLOAD→STORE for an assets plan: the agent finds file URLs,
/// we fetch the bytes ourselves (the agent never handles them) and keep them in
/// the object store. Returns the number of files newly stored.
async fn iterate_assets(
    db: &Db,
    sc: &SourceConfig,
    seed: &Ctx,
    opts: &RuntimeOpts,
    max_new: Option<i64>,
    iteration: i64,
) -> Result<i64> {
    let mut scrape_ctx = seed.clone();
    scrape_ctx.insert("subject".into(), Value::String(sc.subject.clone()));
    if let Some(n) = max_new {
        scrape_ctx.insert("target_remaining".into(), Value::from(n));
    }
    let contract = r#"

Respond with ONLY a fenced ```json``` array of the files you found, each:
{"url": "direct link to the file itself", "title": "what it is", "kind": "pdf|doc|image|other"}
The url must point at the file, not at a page describing it. Return only files
you actually saw linked — do not guess URLs."#;
    let (prompt_source, prompt_label) = match seed.get(PROMPT_HASH_KEY).and_then(Value::as_str) {
        Some(hash) => match store::load_prompt_variant(db, sc.plan_id, hash).await? {
            Some(v) => (format!("{}{}", v.prompt, contract), format!("variant {} ({})", short_hash(hash), v.origin)),
            None => (sc.scrape_prompt.clone(), "stored prompt".to_string()),
        },
        None => (sc.scrape_prompt.clone(), "stored prompt".to_string()),
    };
    let rotation = plan_search_rotation(db, sc, iteration).await;
    let rotation_block = rotation.render();
    scrape_ctx.insert("search_rotation".into(), Value::String(rotation_block.clone()));
    let mut prompt = render_template(&prompt_source, &scrape_ctx).context("render scrape prompt")?;
    if !prompt_source.contains("search_rotation") {
        prompt.push_str(&rotation_block);
    }
    prompt.push_str(&sites_block(sc));
    if !prompt.contains("\"url\"") {
        prompt.push_str(contract);
    }

    println!("\n[1/4 find]   {prompt_label}, prompt {}", human_bytes(prompt.len()));
    println!("  subject    {}", one_line(&sc.subject, 80));
    println!("  rotation   {}", rotation.summary());
    let t = Instant::now();
    let scraped = agent_call(db, "find files", prompt, stage_opts(&opts.agent, "scrape", sc)).await;
    // Read before anything else spends: this is the scrape's own cost.
    let scrape_tokens = crate::meter::last_call_tokens();
    let used_queries = record_search_trail(db, sc.plan_id, &seed_key_of(seed), opts).await;
    let v = scraped.context("find files")?;
    let raw_rows = v.as_array().ok_or_else(|| anyhow!("find files: expected JSON array, got {}", json_type(&v)))?;
    let rows: Vec<Ctx> = raw_rows
        .iter()
        .filter_map(|r| r.as_object())
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .collect();
    println!("  → {} candidate file(s) in {}", rows.len(), fmt_elapsed(t.elapsed()));

    println!("[2/4 filter] checking URLs against what is already stored");
    let (known_hashes, known_urls) = store::known_asset_keys(db, sc.plan_id).await?;
    let mut queue: Vec<(String, String)> = Vec::new(); // (url, title)
    let (mut dup, mut rejected) = (0usize, 0usize);
    for r in &rows {
        let url = r.get("url").and_then(Value::as_str).unwrap_or_default().trim().to_string();
        let title = crate::normalize::cleanse(r.get("title").and_then(Value::as_str).unwrap_or_default());
        if let Err(why) = crate::assets::check_url(&url) {
            rejected += 1;
            println!("     ✗ rejected {}: {why}", one_line(&url, 60));
            continue;
        }
        if known_urls.contains(&url) || queue.iter().any(|(u, _)| u == &url) {
            dup += 1;
            continue;
        }
        queue.push((url, title));
    }
    println!("  → {} to fetch · {dup} already stored · {rejected} rejected", queue.len());
    crate::meter::note_rows(rows.len() as i64, dup as i64, rejected as i64, 0);

    let cap = crate::assets::max_files_per_run();
    let limit = max_new.map(|m| m.max(0) as usize).unwrap_or(cap).min(cap);
    if queue.len() > limit {
        let dropped = queue.len() - limit;
        queue.truncate(limit);
        println!("  → capping to {limit} file(s) (dropping {dropped})");
    }
    if queue.is_empty() {
        println!("[3/4 fetch]  nothing to do");
        println!("[4/4 store]  nothing to store");
        return Ok(0);
    }

    println!("[3/4 fetch]  downloading {} file(s), max {} MB each", queue.len(), crate::assets::max_bytes() / (1024 * 1024));
    let mut stored = 0i64;
    for (i, (url, title)) in queue.iter().enumerate() {
        println!("  [{}/{}] {}", i + 1, queue.len(), one_line(url, 70));
        let t = Instant::now();
        let file = match crate::assets::fetch(url).await {
            Ok(f) => f,
            Err(e) => {
                println!("        ✗ {}", one_line(&format!("{e:#}"), 100));
                continue;
            }
        };
        if known_hashes.contains(&file.sha256) {
            println!("        = identical to a file already stored, skipped");
            continue;
        }
        let key = crate::objstore::object_key(sc.account_id, sc.plan_id, &file.sha256, &file.ext);
        if let Err(e) = crate::objstore::put(&key, &file.bytes, &file.content_type).await {
            println!("        ✗ could not store bytes: {e:#}");
            continue;
        }
        let title = if title.is_empty() { file.filename.clone() } else { title.clone() };
        store::upsert_asset(
            db,
            sc.account_id,
            sc.plan_id,
            current_execution_id(),
            &store::NewAsset {
                source_key: file.sha256.clone(),
                title,
                source_url: url.clone(),
                filename: file.filename.clone(),
                content_type: file.content_type.clone(),
                size_bytes: file.bytes.len() as i64,
                object_key: key,
                metadata: serde_json::to_value(&rows.get(i)).unwrap_or(Value::Null),
            },
        )
        .await
        .context("upsert asset")?;
        stored += 1;
        println!(
            "        ✓ {} · {} · {}",
            one_line(&file.filename, 40),
            human_bytes(file.bytes.len()),
            fmt_elapsed(t.elapsed())
        );
    }
    println!("[4/4 store]  {stored} file(s) written");
    crate::meter::note_rows(0, 0, 0, stored);
    let _ = store::attribute_query_yield(db, sc.plan_id, &used_queries, stored, scrape_tokens).await;
    Ok(stored)
}

fn seed_key_of(seed: &Ctx) -> String {
    crate::sha1_hex(&serde_json::to_string(seed).unwrap_or_default())
}

async fn record_search_trail(db: &Db, plan_id: i64, seed_key: &str, opts: &RuntimeOpts) -> Vec<String> {
    let (searches, pages) = trail::drain();
    let mut keys: Vec<String> = Vec::new();
    let execution_id = current_execution_id();
    for s in &searches {
        if store::record_search_query(db, plan_id, &s.key, &s.query, &s.engine, s.depth).await.is_ok() && !keys.contains(&s.key) {
            keys.push(s.key.clone());
        }
        // The thought that led here: this seed asked for that search.
        if !seed_key.is_empty() {
            let _ = store::record_edge(db, plan_id, execution_id, "seed", seed_key, "query", &s.key).await;
        }
    }
    for p in &pages {
        let _ = store::record_visited_page(db, plan_id, &p.key, &p.url, &p.host).await;
        // The edge is the whole point of the graph: this page came from that
        // search. Without it the record is two lists. A page opened with no
        // search on screen still hangs off the seed — the thought that sent it.
        if let Some(q) = &p.from_query {
            let _ = store::record_edge(db, plan_id, execution_id, "query", q, "page", &p.key).await;
        } else if !seed_key.is_empty() {
            let _ = store::record_edge(db, plan_id, execution_id, "seed", seed_key, "page", &p.key).await;
        }
    }
    let _ = store::prune_search_trail(db, plan_id).await;
    if opts.progress != Level::Off && !(searches.is_empty() && pages.is_empty()) {
        let deepest = searches.iter().map(|s| s.depth).max().unwrap_or(0);
        println!(
            "  trail      {} search(es) (deepest result page {deepest}) · {} page(s) recorded",
            searches.len(),
            pages.len()
        );
    }
    keys
}

pub async fn plan_search_rotation(db: &Db, sc: &SourceConfig, iteration: i64) -> trail::Rotation {
    let queries = store::list_search_queries(db, sc.plan_id, 60).await.unwrap_or_default();
    let pages = store::list_visited_pages(db, sc.plan_id, 200).await.unwrap_or_default();

    let kept: HashSet<String> = safe_replay(queries.iter().map(|q| q.query.clone()).collect(), "recorded searches")
        .into_iter()
        .collect();
    let queries: Vec<_> = queries.into_iter().filter(|q| kept.contains(&q.query)).collect();

    let kept: HashSet<String> = safe_replay(pages.iter().map(|p| p.url.clone()).collect(), "recorded pages")
        .into_iter()
        .collect();
    let pages: Vec<_> = pages.into_iter().filter(|p| kept.contains(&p.url)).collect();

    trail::plan_rotation(&queries, &pages, trail::run_seed(iteration))
}

async fn register_variant(db: &Db, plan_id: i64, prompt: &str) -> Result<String> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        bail!("empty prompt");
    }
    if prompt.len() > MAX_VARIANT_PROMPT_BYTES {
        bail!("prompt is {} — over the {} ceiling", human_bytes(prompt.len()), human_bytes(MAX_VARIANT_PROMPT_BYTES));
    }
    let hash = crate::sha1_hex(prompt);
    store::save_prompt_variant(db, plan_id, &hash, prompt, "agent").await?;
    Ok(hash)
}

const FREE_AGENT_PLANNER_ADDENDUM: &str = r#"

=== FREE AGENT MODE ===
You may also rewrite the scrape prompt itself, not just the search variables.

For any proposal where a different search STRATEGY (not just a different
city/segment) would find prospects the current prompt keeps missing, include
an extra key "_scrape_prompt" whose value is a COMPLETE replacement scrape
prompt. Write the whole prompt, not a diff.

Rules for a replacement prompt:
  1. Keep the same target profile and quality bar as the current prompt.
  2. Do NOT specify the output JSON schema — the pipeline appends the
     required output contract automatically. Concentrate on WHO to look for
     and HOW to find them.
  3. You may use {{.var}} placeholders for any seed variable in your proposal.
  4. Change the search strategy meaningfully: new sources, new phrasing, a
     different angle. A reworded copy of the current prompt is wasted effort.

The current scrape prompt is:
---
{{.current_scrape_prompt}}
---

Prompt variants already tried (hash, origin, runs, new prospects):
{{.prompt_history_csv}}

Proposals without "_scrape_prompt" simply reuse the current prompt."#;

fn safe_replay(values: Vec<String>, what: &str) -> Vec<String> {
    let (kept, dropped) = guard::sanitize_replayed(values);
    if !dropped.is_empty() {
        eprintln!(
            "  ! dropped {} {what} that read as instructions rather than data \
             (a scraped page may be trying to steer later runs): {}",
            dropped.len(),
            dropped.iter().map(|d| format!("{:?}", one_line(d, 80))).collect::<Vec<_>>().join(", ")
        );
    }
    kept
}

async fn resume_queue(db: &Db, plan_id: i64, limit: i64) -> Vec<Ctx> {
    let rows = match store::pending_seeds(db, plan_id, limit.max(1)).await {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("[resume] could not read the seed queue ({e:#}); starting from the plan seed");
            return Vec::new();
        }
    };
    rows.into_iter()
        .filter_map(|(_, json)| match serde_json::from_str::<Ctx>(&json) {
            Ok(ctx) => Some(ctx),
            Err(e) => {
                eprintln!("[resume] skipping unreadable queued seed ({e})");
                None
            }
        })
        .collect()
}

async fn plan_next_seeds(db: &Db, sc: &SourceConfig, original_seed: &Ctx, opts: &RuntimeOpts) -> Result<Vec<Ctx>> {
    let known = safe_replay(
        store::recent_entities(db, sc.plan_id, sc.known_limit, &sc.audience()).await.unwrap_or_default(),
        known_noun(sc),
    );
    let explored = safe_replay(store::explored_seeds(db, sc.plan_id, 50).await.unwrap_or_default(), "explored seeds");
    if opts.progress != Level::Off {
        println!("            context: {} {}, {} explored seeds", known.len(), known_noun(sc), explored.len());
    }

    let mut ctx = original_seed.clone();
    ctx.insert("plan_type".into(), Value::String(sc.audience()));
    insert_known_entities(&mut ctx, known);
    ctx.insert("explored_seeds_csv".into(), Value::String(explored.join(" | ")));
    ctx.insert("explored_seeds".into(), Value::Array(explored.into_iter().map(Value::String).collect()));
    ctx.insert("source".into(), Value::String(sc.source.clone()));

    let mut prompt = render_template(&sc.planner_prompt, &ctx)?;
    if opts.free_agent {
        prompt.push_str(
            &FREE_AGENT_PLANNER_ADDENDUM
                .replace("{{.current_scrape_prompt}}", &sc.scrape_prompt)
                .replace("{{.prompt_history_csv}}", &prompt_history_csv(db, sc.plan_id).await),
        );
    }
    let v = agent_call(db, "planner", prompt, stage_opts(&opts.agent, "planner", sc)).await?;
    let arr = v.as_array().ok_or_else(|| anyhow!("planner: expected JSON array, got {}", json_type(&v)))?;
    Ok(arr
        .iter()
        .filter_map(|item| item.as_object())
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .collect())
}

async fn prompt_history_csv(db: &Db, plan_id: i64) -> String {
    let variants = store::list_prompt_variants(db, plan_id).await.unwrap_or_default();
    if variants.is_empty() {
        return "(none yet — the current prompt has no recorded runs)".into();
    }
    variants
        .iter()
        .take(20)
        .map(|v| format!("{}, {}, {} runs, {} new prospects", short_hash(&v.prompt_hash), v.origin, v.executions, v.new_prospects))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The "start here" line for a plan that named its own sites.
///
/// Appended at run time rather than baked into the stored ScrapePrompt, so
/// editing the site list on the plan takes effect on the next run instead of
/// needing the plan rebuilt. Worded as a preference: these are the places to
/// look first, not a fence — `AllowHosts` is the fence, and being off it ends
/// the run.
fn sites_block(sc: &SourceConfig) -> String {
    let sites = crate::guard::split_sites(&sc.sites);
    if sites.is_empty() {
        return String::new();
    }
    format!(
        "\n\nSTART WITH THESE SITES — the user picked them:\n  {}\nSearch and page through these first, including their own search and listing\npages. Go elsewhere only once you have exhausted them, or if one of them\nblocks you or has nothing matching.\n",
        sites.join("\n  ")
    )
}

async fn enrich_row(
    db: &Db,
    label: &str,
    enrich_prompt: &str,
    seed: &Ctx,
    row: &mut Ctx,
    agent_opts: AgentOpts,
    empty_only: bool,
) -> Result<usize> {
    let ctx = merge_ctx(seed, row);
    let prompt = render_template(enrich_prompt, &ctx)?;
    let v = agent_call(db, label, prompt, agent_opts).await?;
    let obj = match &v {
        Value::Object(m) => Some(m.clone()),
        Value::Array(arr) => arr.first().and_then(|f| f.as_object()).cloned(),
        _ => None,
    };
    let mut filled = 0;
    if let Some(obj) = obj {
        for (k, val) in obj {
            if matches!(&val, Value::String(s) if s.is_empty()) {
                continue;
            }
            if empty_only && !value_blank(row.get(&k)) {
                continue;
            }
            row.insert(k, val);
            filled += 1;
        }
    }
    Ok(filled)
}

fn value_blank(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.trim().is_empty(),
        Some(Value::Array(a)) => a.is_empty(),
        Some(Value::Object(o)) => o.is_empty(),
        _ => false,
    }
}

fn merge_ctx(vars: &Ctx, row: &Ctx) -> Ctx {
    let mut out = vars.clone();
    for (k, v) in row {
        out.insert(k.clone(), v.clone());
    }
    out
}

fn json_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_money_thresholds() {
        assert_eq!(human_money(2_500_000_000), "$2.5B");
        assert_eq!(human_money(1_500_000), "$1.5M");
        assert_eq!(human_money(75_000), "$75K");
        assert_eq!(human_money(999), "$999");
    }

    #[test]
    fn a_filled_scrape_field_counts_as_not_blank() {
        assert!(value_blank(None));
        assert!(value_blank(Some(&Value::String("".into()))));
        assert!(!value_blank(Some(&Value::String("2024 Subaru Legacy".into()))));
        assert!(!value_blank(Some(&Value::from(19900))));
    }

    #[test]
    fn a_run_log_never_calls_a_row_a_prospect() {
        let artifacts = SourceConfig { kind: "artifacts".into(), ..Default::default() };
        let people = SourceConfig { kind: "prospects".into(), ..Default::default() };
        let files = SourceConfig { kind: "assets".into(), ..Default::default() };
        assert_eq!(result_noun(&artifacts, 3), "artifacts");
        assert_eq!(result_noun(&people, 1), "artifact");
        assert_eq!(result_noun(&files, 2), "files");
        assert!(!result_noun(&artifacts, 4).contains("prospect"));
        assert!(!result_noun(&people, 4).contains("prospect"));
    }
}
