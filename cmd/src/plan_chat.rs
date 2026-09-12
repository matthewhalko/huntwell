//! Cursor-backed plan authoring chat for the create-plan wizard.
//!
//! Text-only: no browser, no tab cleanup. The model returns a chat reply
//! plus an optional SourceConfig draft as JSON.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::{ask_agent, AgentOpts};
use crate::progress::Level;
use crate::store::{self, SourceConfig};

/// Default enrich prompt for new **business** plans (contact/person lookup).
pub const DEFAULT_ENRICH_PROMPT: &str = r#"Find the best business decision-maker and a reliable contact at this company.

Company:   {{.company}}  ({{.company_domain}})
Location:  {{.hq_location}}
Known contact so far: {{.contact_name}} — {{.contact_title}}

Use the browser to:
  1. Read the company's own site: leadership, team, about and press pages.
  2. Visit {{.website}} — About / Team / Contact pages.
  3. Confirm the person currently works there when possible.

EMAILS — found-only, never guess:
  - Set contact_email ONLY if you see the address published on the company
    website, its press/about page, or a public professional directory.
  - Do NOT invent pattern emails (first.last@domain), do NOT infer from
    naming conventions, and do NOT use email-finder / verification APIs.
  - If no published address: contact_email "" and email_status "".
  - If you found a personal published address: email_status "verified".
  - If you only found a generic inbox (info@, contact@, press@): you may
    return it with email_status "generic".

PRIVACY: business contact info only. No personal/residential data.

Respond with ONLY a fenced ```json``` object. Empty string if unknown — do NOT
fabricate. Keys:
  contact_name     (str)
  contact_title    (str)
  contact_linkedin (str)
  contact_email    (str — published business email only, else "")
  email_status     (str — "verified" | "generic" | "")
  contact_phone    (str — business)
  website          (str)
  hq_location      (str)
  source_notes     (str — 1 sentence on provenance)
"#;

/// Default enrich prompt for new **individual** plans: the prospect is a
/// private person, so the contact details worth finding are their own — home
/// address, personal phone, personal email — rather than a company's.
pub const DEFAULT_ENRICH_PROMPT_INDIVIDUAL: &str = r#"Find reliable personal contact details for this individual.

Person:    {{.full_name}}
Known so far: {{.occupation}} — {{.home_city}}, {{.home_region}}
Profile:   {{.linkedin}}

Use the browser to:
  1. Open the person's own pages first: personal site, public profile, public
     profiles they publish themselves.
  2. Check public directories and public records that publish contact details
     for this kind of person (professional registries and licence lookups,
     electoral/company officer registers where public, phone and address
     directories, local listings).
  3. Confirm you are looking at the SAME person — match on at least two of:
     full name, city, occupation/employer, profile photo, licence number.
     If you cannot confirm identity, return empty fields rather than a guess.

CONTACT DETAILS — found-only, never guess:
  - personal_email, personal_phone and home_address go in ONLY if you actually
    see them published on a page you opened.
  - Do NOT invent pattern emails (first.last@domain), do NOT infer from naming
    conventions, and do NOT use email-finder / verification APIs.
  - A work email is not a personal email. If all you find is a work address,
    put it in work_email and leave personal_email "".
  - home_address: as published. Street level only if published at street level;
    otherwise city/region is fine. Never estimate or interpolate an address.
  - email_status: "verified" when the address is published on the person's own
    page or an official register, "listed" when it comes from a third-party
    directory, "" when you found nothing.

SOURCING: record where each detail came from in source_notes. Skip anything
behind a login, a paywall, or a scraper block. Respect any page that says its
data may not be reused, and skip people who have clearly opted out of listings.
Do NOT collect anything about minors, and do not go looking for health,
financial, religious, or political details.

Respond with ONLY a fenced ```json``` object. Empty string if unknown — do NOT
fabricate. Keys:
  full_name        (str)
  occupation       (str — job title or trade)
  employer         (str — where they work, if public; else "")
  personal_email   (str — published personal address only, else "")
  work_email       (str — only if that is all you found)
  email_status     (str — "verified" | "listed" | "")
  personal_phone   (str — mobile/home, as published)
  home_address     (str — as published; city/region if that is all there is)
  home_city        (str)
  home_region      (str)
  linkedin         (str)
  personal_website (str)
  source_url       (str — the page the details came from)
  source_notes     (str — 1 sentence on provenance and how identity was matched)
"#;

/// Default planner prompt when learn mode is on.
pub const DEFAULT_PLANNER_PROMPT: &str = r#"Propose the next search seeds for this prospecting plan.

Already explored seed combinations:
{{.explored_seeds_csv}}

Companies already captured:
{{.known_companies_csv}}

RULES:
  1. Vary ONE useful axis per proposal (segment, city, channel, keyword, etc.).
  2. Prefer combinations NOT already tried.
  3. Stay on-ICP — do not wander into unrelated markets.

Respond with ONLY a fenced ```json``` array of 2-4 partial --var overrides.
Each MUST be a new combination not in explored_seeds."#;

/// Default planner prompt for individual plans — the axes worth varying are
/// neighbourhoods, life events and directories, not market segments.
pub const DEFAULT_PLANNER_PROMPT_INDIVIDUAL: &str = r#"Propose the next search seeds for this prospecting plan.

Already explored seed combinations:
{{.explored_seeds_csv}}

People already captured:
{{.known_people_csv}}

RULES:
  1. Vary ONE useful axis per proposal (city or neighbourhood, occupation,
     directory or register, life event, age band, keyword).
  2. Prefer combinations NOT already tried.
  3. Stay on-ICP — these must still be the same kind of person.
  4. Do not propose axes that single people out by health, religion, politics,
     ethnicity, or anything about minors.

Respond with ONLY a fenced ```json``` array of 2-4 partial --var overrides.
Each MUST be a new combination not in explored_seeds."#;

/// Quality bar for scrape prompts — same depth as hand-authored plans like
/// mexico_stablecoin_fintech (segments, search angles, persona, JSON schema).
const SCRAPE_QUALITY_GUIDE_BUSINESS: &str = r#"
ScrapePrompt QUALITY BAR (match this depth — do NOT write a thin one-liner):

A good scrape prompt includes ALL of:
  1. Clear ICP / who to find, with concrete segments or personas
  2. Seed-var hooks using {{.var}} (e.g. country, city, segment) when useful
  3. HOW TO SEARCH — several concrete angles (Google queries, directories,
     association lists, partner pages, press)
  4. PERSONA priority for the decision-maker to pick at each company
  5. EXCLUDE — tell the agent to call prospect_known() with every company on
     a results page BEFORE opening any of them, and to open only what comes
     back as new. {{.known_companies_csv}} may follow as a short sample; the
     tool is the authoritative list and the inlined one is truncated
  6. "Do NOT fabricate" / skip unverified rows
  7. Respond with ONLY a fenced ```json``` array and an explicit field schema
  8. A REQUIRED stable dedupe field (usually company_domain as bare root domain)
  9. EMAILS: only include contact_email when found published on a website or
     a public directory — never guess pattern emails (no first.last@domain). Prefer
     leaving email empty over inventing one.
  10. If {{{{.target_remaining}}}} is referenced, ask for at most that many
      companies (the runner also hard-caps storage to the plan target).

Example structure (adapt to the user's ICP — do not copy Mexico/stablecoins
unless that is what they asked for):

  Find <WHO> in {{.country}} that <WHY THEY FIT>, and identify the best
  decision-maker to contact at each.

  TARGET SEGMENTS:
    - ...
  If {{.segment}} is set, bias toward it; else cover all.
  If {{.city}} is set, prioritize that city.

  HOW TO SEARCH (in the browser):
    1. Google: <specific query templates using {{.vars}}>
    2. Ecosystem lists & press: ...
    3. Partner / registry pages: ...
    4. For each company, open its own site and find the PERSONA below.

  PERSONA (priority order): Role A > Role B > Role C.

  EXCLUDE — call prospect_known() with every company on the results page
  before opening any of them; open only the ones it reports as new.
  Recently stored, as a sample:
  {{.known_companies_csv}}

  Do NOT fabricate. Always return company_domain.

  Respond with ONLY a fenced ```json``` array. Each object:
    company, company_domain (REQUIRED), ...contact fields..., source_url, ...
"#;

/// Quality bar for the scrape prompt of an **individual** plan. The prospect
/// is the person themselves, so there is no company domain to dedupe on and
/// the contact details wanted are personal rather than corporate.
const SCRAPE_QUALITY_GUIDE_INDIVIDUAL: &str = r#"
ScrapePrompt QUALITY BAR (match this depth — do NOT write a thin one-liner):

This plan targets PRIVATE INDIVIDUALS, not companies. The row IS a person.
There is no company_domain to dedupe on and the contact details wanted are the
person's own: home address, personal phone, personal email.

A good scrape prompt includes ALL of:
  1. Clear ICP / who to find, described as people — occupation or life
     situation, location, and any qualifying signal
  2. Seed-var hooks using {{.var}} (e.g. country, city, neighborhood,
     occupation) when useful
  3. HOW TO SEARCH — several concrete angles suited to finding PEOPLE:
     Google queries, public registries and licence lookups, professional
     directories, membership and club rosters, local news and event listings,
     public records, personal sites
  4. WHO QUALIFIES — how to tell one of these people from a lookalike, and how
     to confirm identity (match on at least two of: full name, city,
     occupation/employer, photo, licence number)
  5. EXCLUDE — tell the agent to call prospect_known() with every person on a
     results page BEFORE opening any of them, and to open only what comes back
     as new. {{.known_people_csv}} may follow as a short sample; the tool is
     the authoritative list and the inlined one is truncated
  6. "Do NOT fabricate" / skip a person whose identity you could not confirm
  7. Respond with ONLY a fenced ```json``` array and an explicit field schema
  8. A REQUIRED stable dedupe field: person_key — lowercase slug of full name
     plus home city, e.g. "jane-doe-austin-tx". Same person, same key.
  9. PERSONAL CONTACT DETAILS: only include personal_email, personal_phone or
     home_address when actually published on a page you opened. Never guess a
     pattern email, never infer or interpolate an address, never use
     email-finder / people-search APIs. Prefer empty over invented.
  10. If {{{{.target_remaining}}}} is referenced, ask for at most that many
      people (the runner also hard-caps storage to the plan target).
  11. SOURCING & LIMITS: source_url on every row; skip anything behind a login
      or paywall; skip people who have opted out of listings; never collect
      data on minors; do not seek health, financial, religious, political or
      ethnicity details.

Example structure (adapt to the user's ICP):

  Find <WHAT KIND OF PERSON> in {{.city}}, {{.country}} that <WHY THEY FIT>,
  and collect their personal contact details.

  WHO QUALIFIES:
    - ...
  If {{.occupation}} is set, bias toward it; else cover all.
  If {{.neighborhood}} is set, prioritize it.

  HOW TO SEARCH (in the browser):
    1. Google: <specific query templates using {{.vars}}>
    2. Public registries / licence lookups: ...
    3. Directories, rosters, local listings: ...
    4. For each person, open their own pages and confirm identity.

  EXCLUDE — call prospect_known() with every person on the results page
  before opening any of them; open only the ones it reports as new.
  Recently stored, as a sample:
  {{.known_people_csv}}

  Do NOT fabricate. Skip anyone you could not confirm. Always return person_key
  and source_url.

  Respond with ONLY a fenced ```json``` array. Each object:
    person_key (REQUIRED), full_name, occupation, employer, home_address,
    home_city, home_region, personal_email, email_status, personal_phone,
    linkedin, personal_website, source_url, source_notes
"#;

/// What "business" means for the prompts and field templates being authored.
const AUDIENCE_RULES_BUSINESS: &str = r#"
PLAN AUDIENCE: BUSINESS.

The prospects are people in a company context. Everything you author must aim
at company-held details:
  - Address / location = the company's office or HQ, never a home address.
  - Email = a work address at the company's domain (or a published generic
    company inbox), never a personal one.
  - Phone = a company or direct-line work number.
  - Company and company_domain are first-class fields; company_domain is the
    dedupe key.
  - Keep the standing line "business contact info only, no personal or
    residential data" in EnrichPrompt.
  - Field templates: CompanyTmpl and WebsiteTmpl must be filled;
    LocationTmpl is the company location.
"#;

/// What "individual" means for the prompts and field templates being authored.
const AUDIENCE_RULES_INDIVIDUAL: &str = r#"
PLAN AUDIENCE: INDIVIDUAL.

The prospects are private individuals, not companies. Everything you author
must aim at the person's own details:
  - Address / location = their home address as published (street level only if
    published that way; otherwise city and region).
  - Email = their personal email address. A work address is a fallback only,
    and must be labelled as such.
  - Phone = their personal mobile or home number.
  - There is no company_domain. The dedupe key is person_key: a lowercase slug
    of full name plus home city, e.g. "jane-doe-austin-tx". SourceKeyTmpl must
    be {{{{.person_key}}}}.
  - Do NOT carry over the business-plan line about "business contact info
    only" — this plan is explicitly looking for personal contact details.
  - Both ScrapePrompt and EnrichPrompt must still say: publish-only (never
    guess or infer an address, phone or email), confirm identity before
    recording anything, record source_url, skip logins and paywalls, honour
    opt-outs, never collect data on minors, and never seek health, financial,
    religious, political or ethnicity details.
  - Field templates for this audience:
      SourceKeyTmpl = {{{{.person_key}}}}
      NameTmpl      = {{{{.full_name}}}}
      TitleTmpl     = {{{{.occupation}}}}
      CompanyTmpl   = {{{{.employer}}}}   (context only; may be empty)
      EmailTmpl     = {{{{.personal_email}}}}
      PhoneTmpl     = {{{{.personal_phone}}}}
      LocationTmpl  = {{{{.home_address}}}}
      WebsiteTmpl   = {{{{.personal_website}}}}
      LinkedInTmpl  = {{{{.linkedin}}}}
      NotesTmpl     = {{{{.source_notes}}}}
"#;

/// The audience-specific guidance blocks for one plan type.
fn audience_rules(plan_type: &str) -> &'static str {
    if store::is_individual(plan_type) {
        AUDIENCE_RULES_INDIVIDUAL
    } else {
        AUDIENCE_RULES_BUSINESS
    }
}

fn scrape_quality_guide(plan_type: &str) -> &'static str {
    if store::is_individual(plan_type) {
        SCRAPE_QUALITY_GUIDE_INDIVIDUAL
    } else {
        SCRAPE_QUALITY_GUIDE_BUSINESS
    }
}

/// The stock enrich prompt for a plan of this audience.
pub fn default_enrich_prompt(plan_type: &str) -> &'static str {
    if store::is_individual(plan_type) {
        DEFAULT_ENRICH_PROMPT_INDIVIDUAL
    } else {
        DEFAULT_ENRICH_PROMPT
    }
}

/// The stock planner prompt for a plan of this audience.
pub fn default_planner_prompt(plan_type: &str) -> &'static str {
    if store::is_individual(plan_type) {
        DEFAULT_PLANNER_PROMPT_INDIVIDUAL
    } else {
        DEFAULT_PLANNER_PROMPT
    }
}

/// The per-audience half of the "also author these fields" instructions.
fn also_author_block(plan_type: &str) -> &'static str {
    if store::is_individual(plan_type) {
        r#"Also author:
  - EnrichPrompt: personal contact lookup for each scraped person (use
    {{.full_name}}, {{.home_city}}, {{.linkedin}}, etc.). It must ask for the
    person's home address, personal phone and personal email, ban guessed or
    pattern emails and inferred addresses, require identity confirmation and a
    source_url, and set EmailStatusTmpl={{.email_status}}
  - SourceKeyTmpl: {{.person_key}}"#
    } else {
        r#"Also author:
  - EnrichPrompt: contact/person lookup for each scraped company (use
    {{.company}}, {{.company_domain}}, etc.). MUST ban guessed/pattern emails —
    only addresses published on the company's own site; EmailStatusTmpl={{.email_status}}
  - SourceKeyTmpl: usually {{.company_domain}}"#
    }
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // the prompt-tuning chat is no longer exposed in the app; kept because this module tracks the CLI
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // the prompt-tuning chat is no longer exposed in the app; kept because this module tracks the CLI
pub struct PlanChatRequest {
    #[serde(default)]
    pub messages: Vec<ChatMessage>,
    /// Partial draft already in the UI (merged into the next suggestion).
    pub draft: Option<SourceConfig>,
}

#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)] // the prompt-tuning chat is no longer exposed in the app; kept because this module tracks the CLI
pub struct PlanChatResponse {
    pub reply: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft: Option<SourceConfig>,
}

/// Guided-wizard brief → full SourceConfig (Cursor drafts the scrape prompt).
///
/// Serialize as well as Deserialize: the website stores this on the plan row
/// for the planning service to pick up, so it has to survive a round trip
/// through the database, not just an HTTP body.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PlanDraftRequest {
    /// Plain-language ICP / who to find.
    pub icp: String,
    /// `business` | `individual`. Decides which prompt family is authored.
    #[serde(default)]
    pub plan_type: String,
    /// `prospects` (default) | `artifacts`. Artifacts drafts a field schema.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub source_key_tmpl: String,
    #[serde(default)]
    pub seed_vars_json: String,
    #[serde(default = "default_true")]
    pub learn: bool,
    #[serde(default = "default_iterations")]
    pub iterations: i64,
    #[serde(default = "default_target")]
    pub target_prospects: i64,
    /// Columns the user asked for, with what each should contain. Any at all
    /// means this is a database plan and the schema is theirs, not the
    /// model's.
    #[serde(default)]
    pub columns: Vec<crate::artifact::ColumnRequest>,
    /// Websites to search first, as typed. Normalized to hosts before use.
    #[serde(default)]
    pub sites: String,
    /// Kinds this account may create. Inference is clamped to it, so a brief
    /// that reads like a report cannot produce one where reports are off.
    #[serde(default)]
    pub allowed: Vec<String>,
}

fn default_true() -> bool {
    true
}
fn default_iterations() -> i64 {
    3
}
fn default_target() -> i64 {
    5
}

/// Runs one plan-authoring turn against Cursor `agent`.
#[allow(dead_code)] // the prompt-tuning chat is no longer exposed in the app; kept because this module tracks the CLI
pub fn turn(req: &PlanChatRequest) -> Result<PlanChatResponse> {
    if req.messages.is_empty() {
        bail!("messages required");
    }
    let last = req.messages.last().unwrap();
    if last.role != "user" || last.content.trim().is_empty() {
        bail!("last message must be a non-empty user message");
    }

    let prompt = build_prompt(req);
    let opts = AgentOpts {
        force: true,
        progress: Level::Off,
        model: crate::agent::draft_model(),
    };
    let value = ask_agent("plan chat", &prompt, opts)
        .map_err(|e| anyhow!("cursor agent: {e}"))?;

    let reply = value
        .get("reply")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Here's an updated plan draft.")
        .to_string();

    let draft = value
        .get("draft")
        .filter(|d| d.is_object())
        .and_then(|d| merge_draft(req.draft.as_ref(), d).ok());

    Ok(PlanChatResponse { reply, draft })
}

fn suggest_plan_name(icp: &str) -> String {
    let first = icp
        .split(|c: char| matches!(c, '.' | '!' | '?' | '\n'))
        .next()
        .unwrap_or(icp)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let name: String = first.chars().take(60).collect();
    if name.is_empty() {
        "New plan".into()
    } else {
        name
    }
}

/// One-shot: turn a short ICP description + wizard answers into a full plan
/// with a production-quality ScrapePrompt authored by Cursor.
/// Drafts a custom-artifact plan (Kind='artifacts'): the agent proposes the
/// `FieldsSchemaJson` columns and a runnable `ScrapePrompt` for collecting a
/// list of arbitrary items (cars, jobs, rentals…), not people/companies.
fn draft_artifact_from_brief(req: &PlanDraftRequest, icp: &str) -> Result<SourceConfig> {
    let mut base = SourceConfig { kind: "artifacts".into(), ..SourceConfig::default() };
    base.learn = req.learn;
    base.iterations = if req.iterations < 1 { 3 } else { req.iterations as i32 };
    base.target_prospects = if req.target_prospects < 0 { 10 } else { req.target_prospects as i32 };
    if !req.source.trim().is_empty() {
        base.source = req.source.trim().to_string();
    }
    if !req.seed_vars_json.trim().is_empty() {
        base.seed_vars_json = req.seed_vars_json.trim().to_string();
    }
    // Columns the user typed. When there are any, the schema is settled here
    // and the agent is left with the one job it is actually needed for:
    // working out where on the web these things live and how to read them.
    let wanted = crate::artifact::columns_to_schema(&req.columns);
    if !wanted.is_empty() {
        base.fields_schema_json = serde_json::to_string(&wanted).unwrap_or_default();
    }
    let site_brief = sites_brief(&req.sites);
    let column_brief = if wanted.is_empty() {
        String::new()
    } else {
        let mut b = String::from(
            "\nThe columns are FIXED — they were chosen by the user. Do NOT invent,\nrename, drop or reorder them. Author \"FieldsSchemaJson\" as exactly this JSON\nstring, and make the ScrapePrompt collect precisely these keys:\n",
        );
        b.push_str(&base.fields_schema_json);
        b.push_str("\n\nWhat each column means (use this to write the ScrapePrompt):\n");
        for (spec, req_col) in wanted.iter().zip(req.columns.iter()) {
            let note = req_col.prompt.trim();
            if note.is_empty() {
                b.push_str(&format!("- {}: {}\n", spec.key, spec.label));
            } else {
                b.push_str(&format!("- {} ({}): {}\n", spec.key, spec.label, note));
            }
        }
        b
    };

    let prompt = format!(
        r#"You are authoring a Huntwell custom-artifact plan from a short description.
The plan searches the web and collects a LIST of items — NOT people or companies —
for example used cars, job postings, rentals, grants, or products.

CRITICAL — do NOT use browser tools, navigate the web, or call MCP tools.
Answer from knowledge only.

User's description:
---
{icp}
---

{column_brief}{site_brief}
Design the plan:
- "FieldsSchemaJson": a JSON array (returned as a string) of the columns to collect
  for each item. Each column is
  {{"key":"snake_case_key","label":"Human Label","type":"text|longtext|number|money|url|date","role":""}}
  Rules: include exactly one column with "role":"url" (the link to the item's source
  page), one with "role":"title" (a human label), and one with "role":"key" (a stable
  unique id — the source URL, so two listings cannot collapse into one row.
  A VIN or stock number is a separate column, never the key).
  Choose 4–10 useful columns for THIS kind of item (e.g. used cars: make, model,
  year[number], price[money], mileage[number], location, url).
- "ScrapePrompt": a detailed, runnable instruction telling the agent to search the web
  for these items and respond with ONLY a fenced ```json``` array whose objects use
  EXACTLY the FieldsSchemaJson keys. Be specific about good sources and paging.
- "EnrichPrompt" (optional): how to open one item's url and fill columns left empty.
  Never change the key column. A VIN belongs in its own field, not as listing_id.
- "Source": a short human-readable plan name.
- "SeedVarsJSON": a JSON object string of starting vars the ScrapePrompt references
  (e.g. a city or a max price).

Respond with ONLY a fenced ```json``` object (no other prose):
{{
  "reply": "<one sentence summarizing the plan>",
  "draft": {{ "Source": "...", "Kind": "artifacts", "FieldsSchemaJson": "[ ... ]",
              "ScrapePrompt": "...", "EnrichPrompt": "...", "SeedVarsJSON": "{{...}}" }}
}}"#
    );

    let opts = AgentOpts { force: true, progress: Level::Off, model: crate::agent::draft_model() };
    let value = ask_agent("artifact plan draft", &prompt, opts).map_err(|e| anyhow!("cursor agent: {e}"))?;
    let draft_val = value
        .get("draft")
        .filter(|d| d.is_object())
        .ok_or_else(|| anyhow!("agent returned no draft object"))?;
    let mut sc = merge_draft(Some(&base), draft_val)?;

    // The schema may come back as a JSON array value rather than a string.
    if let Some(fs) = draft_val.get("FieldsSchemaJson") {
        sc.fields_schema_json = match fs {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
    }
    // Columns the user typed are not a suggestion. Whatever the model echoed
    // back, the stored schema is theirs — a renamed or dropped column would
    // show up as a silently missing field in every export.
    if !wanted.is_empty() {
        sc.fields_schema_json = serde_json::to_string(&wanted).unwrap_or_default();
    }
    sc.kind = "artifacts".into();
    sc.learn = req.learn;
    sc.iterations = base.iterations;
    sc.target_prospects = base.target_prospects;
    if sc.source.trim().is_empty() {
        sc.source = suggest_plan_name(icp);
    }
    if sc.scrape_prompt.trim().is_empty() {
        bail!("agent returned an empty ScrapePrompt");
    }
    if crate::artifact::parse_schema(&sc.fields_schema_json).is_empty() {
        bail!("agent returned no field schema for the artifact plan");
    }
    Ok(sc)
}

/// "auto" target: one tiny agent call infers what the brief is asking for —
/// Drafts a plan for the two subject-shaped kinds: a written report about one
/// subject, or the files about it. They share a shape — a `Subject` plus a
/// research prompt — so they share a drafter and differ only in the brief the
/// authoring agent is given.
fn draft_subject_from_brief(req: &PlanDraftRequest, icp: &str, kind: store::PlanKind) -> Result<SourceConfig> {
    let mut base = SourceConfig { kind: kind.as_str().into(), ..SourceConfig::default() };
    // Neither kind accumulates across passes: a report is one document, and a
    // file hunt is a single sweep of what is linked.
    base.learn = false;
    base.iterations = 1;
    base.target_prospects = if kind == store::PlanKind::Assets {
        if req.target_prospects < 1 { 25 } else { req.target_prospects as i32 }
    } else {
        0
    };
    if !req.source.trim().is_empty() {
        base.source = req.source.trim().to_string();
    }
    if !req.seed_vars_json.trim().is_empty() {
        base.seed_vars_json = req.seed_vars_json.trim().to_string();
    }

    let task = match kind {
        store::PlanKind::Report => {
            r#"The plan researches ONE subject on the web and writes a single report about it.
Design the plan:
- "Subject": the one company, person, product or topic the report is about, exactly as it
  should be looked up (a proper name, not a sentence).
- "ScrapePrompt": a detailed, runnable instruction telling the agent what to research and
  what the finished document must cover. Name the sections it should write (e.g. overview,
  history, products, people, funding, recent news, risks — choose what suits THIS subject),
  tell it which kinds of sources to prefer (official site, filings, reputable press) and to
  cite what it uses. Tell it to report only what it actually found and to say when
  something is unknown."#
        }
        _ => {
            r#"The plan searches the web for the FILES published about one subject and collects them.
Design the plan:
- "Subject": the one company, person, product or topic the files are about, exactly as it
  should be looked up (a proper name, not a sentence).
- "ScrapePrompt": a detailed, runnable instruction telling the agent which documents to hunt
  for (e.g. annual reports, SEC filings, spec sheets, manuals, press kits, slide decks) and
  where they are usually published (investor-relations pages, EDGAR, official downloads).
  Tell it to return direct links to the files themselves — a URL that downloads the document,
  not a page describing it — and never to guess a URL it has not seen."#
        }
    };

    let prompt = format!(
        r#"You are authoring a Huntwell search plan from a short description.
{task}
- "Source": a short human-readable plan name.
- "SeedVarsJSON": a JSON object string of any starting vars the ScrapePrompt references
  (often just {{}}).

CRITICAL — do NOT use browser tools, navigate the web, or call MCP tools.
Answer from knowledge only.

User's description:
---
{icp}
---

Respond with ONLY a fenced ```json``` object (no other prose):
{{"reply": "one sentence on what you designed",
  "draft": {{"Source": "...", "Subject": "...", "ScrapePrompt": "...", "SeedVarsJSON": "{{}}"}}}}"#
    );

    let opts = AgentOpts { force: true, progress: Level::Off, model: crate::agent::draft_model() };
    let value = ask_agent("plan draft", &prompt, opts).map_err(|e| anyhow!("cursor agent: {e}"))?;
    let draft_val = value.get("draft").cloned().unwrap_or(value.clone());
    let mut sc = merge_draft(Some(&base), &draft_val)?;
    sc.kind = kind.as_str().into();
    if sc.subject.trim().is_empty() {
        // The agent skipped it: the brief itself is the best fallback.
        sc.subject = icp.chars().take(300).collect::<String>().trim().to_string();
    }
    if sc.source.trim().is_empty() {
        sc.source = sc.subject.chars().take(120).collect();
    }
    if sc.scrape_prompt.trim().is_empty() {
        bail!("the agent did not produce a research prompt");
    }
    Ok(sc)
}

/// contacts (prospects, business/individual) or a list of things (artifacts).
/// Falls back to business prospects if the reply is malformed.
fn classify_brief(icp: &str) -> (String, String) {
    let prompt = format!(
        r#"Classify this search brief for a data-collection tool. Reply with ONLY a JSON object, no prose:
{{"kind":"prospects"|"artifacts"|"report"|"assets","plan_type":"business"|"individual"}}

Choose by the SHAPE of the output the user is asking for:
- "prospects": people or companies to CONTACT (leads, roles, firms, professionals).
  plan_type "business" when they are reached in a work context; "individual" for private persons.
- "artifacts": a LIST OF ROWS about many things — cars, jobs, listings, products, properties,
  events, datasets. Many items, each with the same fields.
- "report": ONE WRITTEN DOCUMENT about a single subject. "Tell me everything about X",
  "research this company", "background on this person", "write me a brief on Y".
  The user wants prose and findings, not a table.
- "assets": the actual FILES about a subject — "collect their SEC filings", "download the
  spec sheets", "get every annual report as a PDF". The user wants documents to keep.

The tie-breaker: one subject + prose = report; one subject + files = assets;
many items + columns = artifacts; someone to email = prospects.
plan_type stays "business" unless the brief is clearly about a private individual.

Brief: {icp}"#
    );
    let opts = AgentOpts { force: false, progress: Level::Off, model: crate::agent::draft_model() };
    match ask_agent("target classify", &prompt, opts) {
        Ok(v) => {
            // A whitelist, so an unexpected label degrades to the safe default
            // rather than silently becoming some other kind.
            let kind = match v.get("kind").and_then(|x| x.as_str()).unwrap_or("").to_ascii_lowercase().as_str() {
                "artifacts" => "artifacts",
                "report" => "report",
                "assets" => "assets",
                _ => "prospects",
            };
            let plan_type = store::normalize_plan_type(v.get("plan_type").and_then(|x| x.as_str()).unwrap_or("business"));
            (kind.to_string(), plan_type)
        }
        Err(_) => ("prospects".into(), "business".into()),
    }
}

/// What the drafting model is told about the sites the user picked.
///
/// It matters at draft time as well as run time: knowing the plan will live on
/// cars.com changes which search strings and page shapes the ScrapePrompt is
/// written around.
fn sites_brief(raw: &str) -> String {
    let sites = crate::guard::split_sites(raw);
    if sites.is_empty() {
        return String::new();
    }
    format!(
        "\nThe user named the websites to search: {}.\nWrite the ScrapePrompt around these: their search URLs, their listing pages,\ntheir paging. Treat other sources as a fallback, not the plan.\n",
        sites.join(", ")
    )
}

/// Keeps an inferred kind inside what this account may build.
///
/// A brief that reads like a report, on an account where reports are off, is
/// not an error — the person asked for something, and the nearest thing that
/// works is a table (or, failing that, whatever is on). Only an *explicit*
/// choice of a disabled kind is refused, and that is refused at the API.
fn clamp_kind(kind: String, allowed: &[String]) -> String {
    if allowed.is_empty() || allowed.iter().any(|k| k == &kind) {
        return kind;
    }
    for fallback in ["artifacts", "prospects"] {
        if allowed.iter().any(|k| k == fallback) {
            return fallback.to_string();
        }
    }
    allowed[0].clone()
}

/// The obvious cases, decided without a model call.
///
/// Classification is a whole round trip in front of the draft — the user is
/// watching a spinner through both. Most briefs announce their kind in so many
/// words ("write me a report on…", "cars for sale in…"), and for those the
/// model adds latency and nothing else.
///
/// Deliberately timid: a brief that trips markers for two kinds, or none,
/// returns `None` and goes to the model. Being fast on the easy half is the
/// whole point; guessing on the hard half would cost a wrong plan.
fn quick_kind(icp: &str) -> Option<(String, String)> {
    let t = format!(" {} ", icp.to_ascii_lowercase());
    let hit = |ms: &[&str]| ms.iter().any(|m| t.contains(m));

    let report = hit(&[
        "write me a report", "report on ", "research ", "tell me everything about",
        "background on ", "brief on ", "profile of ", "deep dive", "write up on ",
    ]);
    let assets = hit(&[
        "download ", "pdfs", "pdf files", "sec filings", "spec sheets", "brochures",
        "annual reports", "collect the files", "collect files",
    ]);
    let prospects = hit(&[
        "leads", "prospects", "decision makers", "decision-makers", "contacts at ",
        "email addresses", "hiring managers", "people to email", "who to email",
    ]);
    let artifacts = hit(&[
        "for sale", "listings", "job postings", "used cars", "inventory",
        "prices for", "price list", "tenders", "rfps", "properties in",
    ]);

    match (report, assets, prospects, artifacts) {
        (true, false, false, false) => Some(("report".into(), "business".into())),
        (false, true, false, false) => Some(("assets".into(), "business".into())),
        (false, false, true, false) => Some(("prospects".into(), "business".into())),
        (false, false, false, true) => Some(("artifacts".into(), "business".into())),
        // Nothing, or more than one: the model earns its round trip.
        _ => None,
    }
}

pub fn draft_from_brief(req: &PlanDraftRequest) -> Result<SourceConfig> {
    let icp = req.icp.trim();
    if icp.is_empty() {
        bail!("icp description required");
    }
    // "auto" target: infer kind + plan_type from the brief before drafting.
    let owned;
    // Asking for columns is asking for a table: no classification needed, and
    // no round trip spent deciding something the user already said.
    let req = if !req.columns.is_empty() && matches!(req.kind.trim().to_ascii_lowercase().as_str(), "" | "auto" | "artifacts") {
        let mut r = req.clone();
        r.kind = "artifacts".into();
        owned = r;
        &owned
    } else if req.kind.trim().eq_ignore_ascii_case("auto") {
        let (kind, plan_type) = quick_kind(icp).unwrap_or_else(|| classify_brief(icp));
        let kind = clamp_kind(kind, &req.allowed);
        let mut r = req.clone();
        r.kind = kind;
        if r.plan_type.trim().is_empty() || r.plan_type.trim().eq_ignore_ascii_case("auto") {
            r.plan_type = plan_type;
        }
        owned = r;
        &owned
    } else {
        req
    };
    match store::PlanKind::parse(&req.kind) {
        store::PlanKind::Artifacts => return draft_artifact_from_brief(req, icp),
        store::PlanKind::Report => return draft_subject_from_brief(req, icp, store::PlanKind::Report),
        store::PlanKind::Assets => return draft_subject_from_brief(req, icp, store::PlanKind::Assets),
        store::PlanKind::Prospects => {}
    }

    let plan_type = store::normalize_plan_type(&req.plan_type);
    let mut base = default_draft_for(&plan_type);
    if !req.source.trim().is_empty() {
        base.source = req.source.trim().to_string();
    }
    if !req.source_key_tmpl.trim().is_empty() {
        base.source_key_tmpl = req.source_key_tmpl.trim().to_string();
    }
    if !req.seed_vars_json.trim().is_empty() {
        base.seed_vars_json = req.seed_vars_json.trim().to_string();
    }
    base.learn = req.learn;
    base.iterations = if req.iterations < 1 { 3 } else { req.iterations as i32 };
    base.target_prospects = if req.target_prospects < 0 {
        5
    } else {
        req.target_prospects as i32
    };

    let base_json = serde_json::to_string_pretty(&base).unwrap_or_else(|_| "{}".into());
    let site_brief = sites_brief(&req.sites);
    let audience = audience_rules(&plan_type);
    let scrape_guide = scrape_quality_guide(&plan_type);
    let also_author = also_author_block(&plan_type);
    let prompt = format!(
        r#"You are authoring a huntwell SourceConfig from a short ICP description.
Huntwell runs Cursor agent + Chrome at scrape time; you are only
drafting configuration now.

CRITICAL — do NOT use browser tools, navigate the web, or call MCP tools.
Answer from knowledge only.

User's ICP description:
---
{icp}
---

Wizard preferences (honor these; fill anything missing):
```json
{base_json}
```

{audience}
{site_brief}
{scrape_guide}


{also_author}
  - Source: short human-readable plan name (keep wizard Source if already set)
  - SeedVarsJSON: JSON object string with sensible starting vars for this ICP
    (e.g. country/city/segment keys the scrape prompt references)
  - Field templates mapped to the JSON keys you invent in ScrapePrompt
  - PlanType: "{plan_type}" — do not change it
  - TargetProspects=5, Iterations from wizard, Learn from wizard unless absurd

Respond with ONLY a fenced ```json``` object (no other prose) shaped as:
{{
  "reply": "<one sentence summarizing the plan>",
  "draft": {{ /* complete SourceConfig fields */ }}
}}

The ScrapePrompt must be detailed and runnable — same quality as a hand-written
marketing script, not a stub."#
    );

    let opts = AgentOpts {
        force: true,
        progress: Level::Off,
        model: crate::agent::draft_model(),
    };
    let value = ask_agent("plan draft", &prompt, opts)
        .map_err(|e| anyhow!("cursor agent: {e}"))?;

    let draft_val = value
        .get("draft")
        .filter(|d| d.is_object())
        .ok_or_else(|| anyhow!("agent returned no draft object"))?;
    let mut sc = merge_draft(Some(&base), draft_val)?;
    if sc.scrape_prompt.trim().is_empty() {
        bail!("agent returned an empty ScrapePrompt");
    }
    if sc.source.trim().is_empty() {
        // Fallback name from ICP if the model omitted Source.
        sc.source = suggest_plan_name(icp);
    }
    // Wizard choices win for these knobs. The audience especially: the whole
    // draft was authored against it, so the model does not get to reinterpret
    // a business plan as an individual one (or the reverse) on the way out.
    sc.plan_type = plan_type;
    sc.learn = req.learn;
    sc.iterations = base.iterations;
    sc.target_prospects = base.target_prospects;
    if !req.source_key_tmpl.trim().is_empty() {
        sc.source_key_tmpl = req.source_key_tmpl.trim().to_string();
    }
    Ok(sc)
}

/// The dedupe / email rules that differ by audience, spliced into the chat
/// prompt so the model is told the right ones for the plan being edited.
const CHAT_RULES_BUSINESS: &str = r#"  - ScrapePrompt must require a stable dedupe field (usually company_domain)
    and ask for a fenced ```json``` array of objects.
  - SourceKeyTmpl should usually be `{{.company_domain}}` or `{{.email}}`.
  - EMAIL POLICY: ScrapePrompt + EnrichPrompt must forbid guessed emails.
    Only list contact_email when found on the company's own website. Never
    invent first.last@domain patterns. Set EmailStatusTmpl to
    {{.email_status}} (verified|generic|"")."#;

const CHAT_RULES_INDIVIDUAL: &str = r#"  - ScrapePrompt must require the stable dedupe field person_key (a lowercase
    slug of full name plus home city, e.g. "jane-doe-austin-tx") and ask for a
    fenced ```json``` array of objects.
  - SourceKeyTmpl must be `{{.person_key}}`.
  - CONTACT POLICY: ScrapePrompt + EnrichPrompt must forbid guessed emails and
    inferred addresses. personal_email, personal_phone and home_address go in
    only when published on a page the agent actually opened, with a source_url
    recorded and identity confirmed. Set EmailStatusTmpl to {{.email_status}}
    (verified|listed|"")."#;

/// The `draft` skeleton shown to the model, with the field templates that suit
/// this audience — an individual plan has no company_domain to map.
const CHAT_TMPL_SKELETON_BUSINESS: &str = r#"    "SourceKeyTmpl": "{{.company_domain}}",
    "NameTmpl": "{{.contact_name}}",
    "TitleTmpl": "{{.contact_title}}",
    "CompanyTmpl": "{{.company}}",
    "IndustryTmpl": "",
    "EmailTmpl": "{{.contact_email}}",
    "EmailStatusTmpl": "{{.email_status}}",
    "PhoneTmpl": "{{.contact_phone}}",
    "WebsiteTmpl": "{{.website}}",
    "LinkedInTmpl": "{{.contact_linkedin}}",
    "LocationTmpl": "{{.hq_location}}",
    "NotesTmpl": "","#;

const CHAT_TMPL_SKELETON_INDIVIDUAL: &str = r#"    "SourceKeyTmpl": "{{.person_key}}",
    "NameTmpl": "{{.full_name}}",
    "TitleTmpl": "{{.occupation}}",
    "CompanyTmpl": "{{.employer}}",
    "IndustryTmpl": "",
    "EmailTmpl": "{{.personal_email}}",
    "EmailStatusTmpl": "{{.email_status}}",
    "PhoneTmpl": "{{.personal_phone}}",
    "WebsiteTmpl": "{{.personal_website}}",
    "LinkedInTmpl": "{{.linkedin}}",
    "LocationTmpl": "{{.home_address}}",
    "NotesTmpl": "{{.source_notes}}","#;

#[allow(dead_code)] // the prompt-tuning chat is no longer exposed in the app; kept because this module tracks the CLI
fn build_prompt(req: &PlanChatRequest) -> String {
    let transcript = req
        .messages
        .iter()
        .map(|m| {
            let role = if m.role == "assistant" {
                "assistant"
            } else {
                "user"
            };
            format!("{role}: {}", m.content.trim())
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    // The audience is a plan-level setting the user picked; every guide below
    // is chosen from it rather than inferred from what the chat happens to say.
    let plan_type = req
        .draft
        .as_ref()
        .map(|d| d.audience())
        .unwrap_or_else(|| store::PLAN_TYPE_BUSINESS.to_string());
    let individual = store::is_individual(&plan_type);
    // The chat edits an existing draft, so the sites come from it.
    let site_brief = req.draft.as_ref().map(|d| sites_brief(&d.sites)).unwrap_or_default();
    let audience = audience_rules(&plan_type);
    let scrape_guide = scrape_quality_guide(&plan_type);
    let audience_chat_rules = if individual {
        CHAT_RULES_INDIVIDUAL
    } else {
        CHAT_RULES_BUSINESS
    };
    let tmpl_skeleton = if individual {
        CHAT_TMPL_SKELETON_INDIVIDUAL
    } else {
        CHAT_TMPL_SKELETON_BUSINESS
    };
    let scrape_shape = if individual {
        "ScrapePrompt must be detailed (who qualifies, search angles, identity\ncheck, JSON schema) — never a short stub."
    } else {
        "ScrapePrompt must be detailed (segments, search angles, persona, JSON\nschema) — never a short stub."
    };

    let draft_json = match &req.draft {
        Some(d) => serde_json::to_string_pretty(d).unwrap_or_else(|_| "{}".into()),
        None => "{}".into(),
    };

    format!(
        r#"You are helping author a huntwell SourceConfig (a "plan") for the
huntwell CLI. Huntwell uses Cursor agent + Chrome at *run* time; you
are only drafting configuration now.

CRITICAL — do NOT use browser tools, navigate the web, or call MCP tools.
Answer from knowledge only. This is a text-only configuration session.

{audience}
{site_brief}
Rules for the plan:
  - Templates are ONLY simple `{{{{.field}}}}` interpolation — no {{{{if}}}},
    {{{{range}}}}, pipelines, or functions.
{audience_chat_rules}
  - TargetProspects defaults to 5 unless the user asks otherwise.
  - Iterations default 3, MaxNoProgress 2, KnownLimit 200.
  - Learn defaults to true — always include a PlannerPrompt for seed proposals.
  - FreeAgent defaults to false unless the user asks to mutate scrape prompts.
  - Always include a solid EnrichPrompt for contact lookup (do not leave it empty).
  - Source must be a short human-readable plan name, not a snake_case id.
  - PlanType is "{plan_type}" and must stay that way unless the user explicitly
    asks to switch this plan between business and individual.
  - SeedVarsJSON must be a JSON object string (e.g. "{{}}").
  - Fill sensible NameTmpl/TitleTmpl/CompanyTmpl/EmailTmpl/WebsiteTmpl/
    LocationTmpl/LinkedInTmpl from the scrape field names you invent.

{scrape_guide}


Current draft (may be empty / partial):
```json
{draft_json}
```

Conversation so far:
{transcript}

Respond with ONLY a fenced ```json``` object (no other prose outside the fence)
with this shape:
{{
  "reply": "<short helpful message to the user in plain language>",
  "draft": {{
    "Source": "...",
    "PlanType": "{plan_type}",
    "ScrapePrompt": "...",
    "EnrichPrompt": "...",
    "PlannerPrompt": "...",
{tmpl_skeleton}
    "EstimatedValueTmpl": "",
    "MinValue": 0,
    "Learn": true,
    "Iterations": 3,
    "MaxNoProgress": 2,
    "KnownLimit": 200,
    "TargetProspects": 5,
    "FreeAgent": false,
    "Favorite": false,
    "Model": "",
    "SeedVarsJSON": "{{}}"
  }}
}}

Always include a complete `draft` object with every field filled as best you can
from the conversation. Set TargetProspects to 5 and Learn to true unless the
user requested otherwise. Never leave EnrichPrompt or PlannerPrompt empty.
{scrape_shape}"#
    )
}

/// Merge a JSON object into a base SourceConfig (or defaults).
fn merge_draft(base: Option<&SourceConfig>, patch: &Value) -> Result<SourceConfig> {
    let mut sc = base.cloned().unwrap_or_else(default_draft);
    // The audience the caller was already working with. A patch may change it
    // (the chat lets the user say "these are individuals"), and every default
    // filled in below follows whichever one ends up set.
    sc.plan_type = sc.audience();
    let obj = patch
        .as_object()
        .ok_or_else(|| anyhow!("draft must be an object"))?;

    macro_rules! str_field {
        ($field:ident, $key:expr) => {
            if let Some(v) = obj.get($key).and_then(Value::as_str) {
                sc.$field = v.to_string();
            }
        };
    }
    macro_rules! i64_field {
        ($field:ident, $key:expr) => {
            if let Some(v) = obj.get($key).and_then(|x| {
                x.as_i64()
                    .or_else(|| x.as_f64().map(|f| f as i64))
                    .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
            }) {
                sc.$field = v as _;
            }
        };
    }
    macro_rules! bool_field {
        ($field:ident, $key:expr) => {
            if let Some(v) = obj.get($key).and_then(|x| {
                x.as_bool().or_else(|| match x.as_str() {
                    Some("true") | Some("1") => Some(true),
                    Some("false") | Some("0") => Some(false),
                    _ => None,
                })
            }) {
                sc.$field = v;
            }
        };
    }

    str_field!(source, "Source");
    str_field!(plan_type, "PlanType");
    str_field!(subject, "Subject");
    str_field!(scrape_prompt, "ScrapePrompt");
    str_field!(enrich_prompt, "EnrichPrompt");
    str_field!(planner_prompt, "PlannerPrompt");
    str_field!(source_key_tmpl, "SourceKeyTmpl");
    str_field!(name_tmpl, "NameTmpl");
    str_field!(title_tmpl, "TitleTmpl");
    str_field!(company_tmpl, "CompanyTmpl");
    str_field!(industry_tmpl, "IndustryTmpl");
    str_field!(email_tmpl, "EmailTmpl");
    str_field!(email_status_tmpl, "EmailStatusTmpl");
    str_field!(phone_tmpl, "PhoneTmpl");
    str_field!(website_tmpl, "WebsiteTmpl");
    str_field!(linkedin_tmpl, "LinkedInTmpl");
    str_field!(location_tmpl, "LocationTmpl");
    str_field!(notes_tmpl, "NotesTmpl");
    str_field!(estimated_value_tmpl, "EstimatedValueTmpl");
    str_field!(seed_vars_json, "SeedVarsJSON");
    i64_field!(min_value, "MinValue");
    i64_field!(iterations, "Iterations");
    i64_field!(max_no_progress, "MaxNoProgress");
    i64_field!(known_limit, "KnownLimit");
    i64_field!(target_prospects, "TargetProspects");
    bool_field!(learn, "Learn");
    bool_field!(free_agent, "FreeAgent");
    bool_field!(favorite, "Favorite");
    str_field!(model, "Model");

    sc.plan_type = store::normalize_plan_type(&sc.plan_type);
    let individual = store::is_individual(&sc.plan_type);
    if sc.source_key_tmpl.trim().is_empty() {
        sc.source_key_tmpl = if individual {
            "{{.person_key}}".into()
        } else {
            "{{.company_domain}}".into()
        };
    }
    if sc.seed_vars_json.trim().is_empty() {
        sc.seed_vars_json = "{}".into();
    }
    // Validate seed JSON.
    let _: Value = serde_json::from_str(&sc.seed_vars_json)
        .map_err(|e| anyhow!("SeedVarsJSON: {e}"))?;
    if sc.iterations < 1 {
        sc.iterations = 3;
    }
    if sc.max_no_progress < 1 {
        sc.max_no_progress = 2;
    }
    if sc.known_limit < 0 {
        sc.known_limit = 200;
    }
    if sc.target_prospects <= 0 {
        sc.target_prospects = 5;
    }
    if sc.enrich_prompt.trim().is_empty() {
        sc.enrich_prompt = default_enrich_prompt(&sc.plan_type).into();
    }
    if sc.learn && sc.planner_prompt.trim().is_empty() {
        sc.planner_prompt = default_planner_prompt(&sc.plan_type).into();
    }

    Ok(sc)
}


/// The stock business draft. Kept as the no-argument entry point because a
/// caller with no audience in hand is asking for the historical default.
pub fn default_draft() -> SourceConfig {
    default_draft_for(store::PLAN_TYPE_BUSINESS)
}

/// A blank draft whose prompts and field templates already match `plan_type`,
/// so a plan created without ever talking to the model is still coherent.
pub fn default_draft_for(plan_type: &str) -> SourceConfig {
    let plan_type = store::normalize_plan_type(plan_type);
    let individual = store::is_individual(&plan_type);
    let (source_key, name, title, company, email, phone, website, linkedin, location, notes) =
        if individual {
            (
                "{{.person_key}}",
                "{{.full_name}}",
                "{{.occupation}}",
                "{{.employer}}",
                "{{.personal_email}}",
                "{{.personal_phone}}",
                "{{.personal_website}}",
                "{{.linkedin}}",
                "{{.home_address}}",
                "{{.source_notes}}",
            )
        } else {
            (
                "{{.company_domain}}",
                "{{.contact_name}}",
                "{{.contact_title}}",
                "{{.company}}",
                "{{.contact_email}}",
                "{{.contact_phone}}",
                "{{.website}}",
                "{{.contact_linkedin}}",
                "{{.hq_location}}",
                "",
            )
        };
    SourceConfig {
        source: String::new(),
        scrape_prompt: String::new(),
        enrich_prompt: default_enrich_prompt(&plan_type).into(),
        planner_prompt: default_planner_prompt(&plan_type).into(),
        source_key_tmpl: source_key.into(),
        name_tmpl: name.into(),
        title_tmpl: title.into(),
        company_tmpl: company.into(),
        industry_tmpl: String::new(),
        email_tmpl: email.into(),
        email_status_tmpl: "{{.email_status}}".into(),
        phone_tmpl: phone.into(),
        website_tmpl: website.into(),
        linkedin_tmpl: linkedin.into(),
        location_tmpl: location.into(),
        notes_tmpl: notes.into(),
        estimated_value_tmpl: String::new(),
        min_value: 0,
        learn: true,
        iterations: 3,
        max_no_progress: 2,
        known_limit: 200,
        target_prospects: 5,
        free_agent: false,
        favorite: false,
        model: String::new(),
        seed_vars_json: "{}".into(),
        plan_type,
        // New plans start unscheduled; the editor's Schedule section sets this.
        schedule_enabled: false,
        schedule_time: String::new(),
        schedule_days: String::new(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn quick_kind_takes_the_obvious_cases() {
        assert_eq!(quick_kind("write me a report on Stripe's pricing").unwrap().0, "report");
        assert_eq!(quick_kind("find me crosstreks for sale in reno").unwrap().0, "artifacts");
        assert_eq!(quick_kind("treasury leads at mexican fintechs").unwrap().0, "prospects");
        assert_eq!(quick_kind("download the sec filings for every listed bank").unwrap().0, "assets");
    }

    #[test]
    fn quick_kind_defers_when_it_is_not_obvious() {
        // No marker at all.
        assert!(quick_kind("coffee roasters in portland").is_none());
        // Two kinds at once: a list of listings, but also asking for leads.
        assert!(quick_kind("leads from car listings for sale").is_none());
        assert!(quick_kind("").is_none());
    }
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_applies_defaults() {
        let patch = json!({
            "Source": "demo_plan",
            "ScrapePrompt": "find stuff",
            "TargetProspects": 5
        });
        let sc = merge_draft(None, &patch).unwrap();
        assert_eq!(sc.source, "demo_plan");
        assert_eq!(sc.source_key_tmpl, "{{.company_domain}}");
        assert_eq!(sc.target_prospects, 5);
        assert_eq!(sc.iterations, 3);
        assert!(sc.learn);
        assert!(!sc.enrich_prompt.is_empty());
        assert!(!sc.planner_prompt.is_empty());
    }

    #[test]
    fn the_default_enrich_prompt_is_contact_lookup_only() {
        // Outreach drafting is gone: enrichment finds contact details and
        // stops. Anything that reintroduces email copy should fail here.
        for p in [DEFAULT_ENRICH_PROMPT, DEFAULT_ENRICH_PROMPT_INDIVIDUAL] {
            let p = p.to_lowercase();
            assert!(!p.contains("cold_email"), "no outreach drafting");
            assert!(!p.contains("linkedin_message"), "no outreach drafting");
        }
        assert!(DEFAULT_ENRICH_PROMPT.to_lowercase().contains("contact_email"));
    }

    #[test]
    fn merging_a_patch_keeps_the_audience_and_fills_its_defaults() {
        // A model that returns only the two required prompts still gets the
        // rest of an individual plan, not the business fallbacks.
        let base = default_draft_for("individual");
        let patch = json!({
            "Source": "Austin sellers",
            "ScrapePrompt": "find people",
            "EnrichPrompt": "",
            "PlannerPrompt": "",
            "SourceKeyTmpl": "",
        });
        let sc = merge_draft(Some(&base), &patch).unwrap();
        assert_eq!(sc.plan_type, "individual");
        assert_eq!(sc.source_key_tmpl, "{{.person_key}}");
        assert!(sc.enrich_prompt.contains("home_address"));
        assert!(sc.planner_prompt.contains("known_people_csv"));

        // An unknown audience is business, never a silent third mode.
        let odd = merge_draft(None, &json!({"PlanType": "whatever", "ScrapePrompt": "x"})).unwrap();
        assert_eq!(odd.plan_type, "business");
    }

    #[test]
    fn the_chat_prompt_is_written_for_the_drafts_audience() {
        let req = |plan_type: &str| PlanChatRequest {
            messages: vec![ChatMessage {
                role: "user".into(),
                content: "find me some leads".into(),
            }],
            draft: Some(default_draft_for(plan_type)),
        };
        let individual = build_prompt(&req("individual"));
        assert!(individual.contains("PLAN AUDIENCE: INDIVIDUAL"));
        assert!(individual.contains("person_key"));
        assert!(!individual.contains("PLAN AUDIENCE: BUSINESS"));

        let business = build_prompt(&req("business"));
        assert!(business.contains("PLAN AUDIENCE: BUSINESS"));
        assert!(business.contains("company_domain"));
        // "never a home address" is a business rule; asking for one is not.
        assert!(!business.contains("personal_email"));
        assert!(!business.contains("person_key"));
    }

    #[test]
    fn suggest_plan_name_keeps_readable_words() {
        assert_eq!(
            suggest_plan_name("CFOs at mid-market SaaS companies in Texas. Extra."),
            "CFOs at mid-market SaaS companies in Texas"
        );
        assert_eq!(suggest_plan_name("   "), "New plan");
    }
}

#[cfg(test)]
mod clamp_tests {
    use super::clamp_kind;

    fn kinds(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_allowed_kind_survives() {
        assert_eq!(clamp_kind("report".into(), &kinds(&["prospects", "report"])), "report");
    }

    #[test]
    fn a_disabled_kind_becomes_the_nearest_thing_that_works() {
        // Somebody asked for a report on an account without them: a table is
        // an answer, an error is not.
        assert_eq!(clamp_kind("report".into(), &kinds(&["prospects", "artifacts"])), "artifacts");
        assert_eq!(clamp_kind("assets".into(), &kinds(&["prospects"])), "prospects");
    }

    #[test]
    fn no_list_means_no_clamping() {
        // An empty list is "not configured", not "nothing allowed" — the API
        // is where an explicit choice is refused.
        assert_eq!(clamp_kind("assets".into(), &[]), "assets");
    }
}
