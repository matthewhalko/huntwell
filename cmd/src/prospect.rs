//! Prospect mapping: projecting a raw scraped+enriched row into the canonical
//! Prospect schema via `{{.field}}` templates (Go text/template-style field
//! interpolation, the only construct the stored configs use).

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Result};
use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;

use crate::normalize::{cleanse, cleanse_key, smart_title_case};

pub type Ctx = BTreeMap<String, Value>;

/// Mirrors the SQL Server [dbo].[Prospect] table.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct Prospect {
    pub name: String,
    pub title: String,
    pub company: String,
    pub industry: String,
    pub email: String,
    pub email_status: String, // "verified" | "generic" | ""
    pub phone: String,
    pub website: String,
    pub linkedin: String,
    pub location: String,
    pub notes: String,
    pub estimated_value: Option<i64>, // nullable
    pub source: String,
    pub metadata: String, // JSON blob
    pub source_key: String,
}

/// Holds the templates used to project a raw scraped+enriched row into the
/// canonical Prospect schema. Each field is a `{{.key}}` template string
/// evaluated against the runtime vars merged with the row.
#[derive(Debug, Clone, Default)]
pub struct Mapping {
    pub source: String,     // value for Source column
    pub source_key: String, // template; required
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
    pub estimated_value: String, // template that should render to an integer
}

impl Mapping {
    /// Turns a single row into a Prospect using the mapping templates.
    /// `ctx` is the merged set of runtime --var values and the row itself.
    ///
    /// Display-oriented fields (Name, Title, Company, Industry, Location) are
    /// run through smart_title_case, which cleanses the characters and then
    /// title-cases ALL-CAPS scraped values so "COTTONWOOD MANAGEMENT LLC"
    /// comes out as "Cottonwood Management LLC". Values that already contain
    /// mixed case keep their casing but are still cleansed.
    ///
    /// Phone and Website are cleansed but never re-cased — case folding would
    /// corrupt URLs, and title-casing a phone number is meaningless.
    ///
    /// SourceKey gets only the lossless `cleanse_key` treatment: it must stay
    /// byte-stable for dedupe, so no punctuation is rewritten, but invisible
    /// characters are still removed — one zero-width space in a key would
    /// otherwise mean that prospect never matches itself again.
    pub fn map(&self, row: &Ctx, ctx: &Ctx) -> Result<Prospect> {
        let rend = |tmpl: &str| -> Result<String> {
            if tmpl.is_empty() {
                return Ok(String::new());
            }
            Ok(render_template(tmpl, ctx)?.trim().to_string())
        };

        let mut p = Prospect {
            source: self.source.clone(),
            ..Default::default()
        };

        p.source_key = cleanse_key(&rend(&self.source_key).map_err(|e| anyhow::anyhow!("source-key: {e}"))?);
        if p.source_key.is_empty() {
            bail!("source-key rendered to empty for row: {:?}", row);
        }

        p.name = smart_title_case(&rend(&self.name)?);
        p.title = smart_title_case(&rend(&self.title)?);
        p.company = smart_title_case(&rend(&self.company)?);
        p.industry = smart_title_case(&rend(&self.industry)?);
        // Email, LinkedIn and Notes are treated like Phone/Website: cleansed,
        // never re-cased — case folding would corrupt addresses and URLs, and
        // Notes are free-form sentences, not display names.
        p.email = cleanse(&rend(&self.email)?);
        p.email_status = cleanse(&rend(&self.email_status)?).to_lowercase();
        p.phone = cleanse(&rend(&self.phone)?);
        p.website = cleanse(&rend(&self.website)?);
        p.linkedin = cleanse(&rend(&self.linkedin)?);
        p.location = smart_title_case(&rend(&self.location)?);
        p.notes = cleanse(&rend(&self.notes)?);

        let ev_str = rend(&self.estimated_value)?;
        if !ev_str.is_empty() {
            let cleaned = strip_money(&ev_str);
            if let Ok(n) = cleaned.parse::<i64>() {
                p.estimated_value = Some(n);
            } else if let Ok(f) = cleaned.parse::<f64>() {
                p.estimated_value = Some(f as i64);
            }
        }

        // MetaData = JSON of the raw row.
        p.metadata = serde_json::to_string(row).unwrap_or_else(|_| "{}".into());
        Ok(p)
    }

    fn all_templates(&self) -> [&str; 12] {
        [
            &self.source_key,
            &self.name,
            &self.title,
            &self.company,
            &self.industry,
            &self.email,
            &self.email_status,
            &self.phone,
            &self.website,
            &self.linkedin,
            &self.location,
            &self.notes,
        ]
    }

    /// Every `{{.field}}` name referenced by any mapping template.
    pub fn referenced_fields(&self) -> Vec<String> {
        let mut set = BTreeSet::new();
        for tmpl in self.all_templates() {
            set.extend(fields_in(tmpl));
        }
        set.extend(fields_in(&self.estimated_value));
        set.into_iter().collect()
    }

    /// Field names the SourceKey template depends on — these must never be
    /// empty, since `map` refuses a row whose key renders blank.
    pub fn key_fields(&self) -> Vec<String> {
        fields_in(&self.source_key).into_iter().collect()
    }

    /// Names the scraped row itself has to supply: everything referenced,
    /// minus seed vars and the variables the pipeline injects.
    pub fn row_fields(&self, seed_keys: &BTreeSet<String>) -> Vec<String> {
        self.referenced_fields()
            .into_iter()
            .filter(|f| !is_supplied(f, seed_keys))
            .collect()
    }
}

/// Template variables supplied by the pipeline rather than the scrape.
const INJECTED_VARS: [&str; 11] = [
    "known_companies",
    "known_companies_csv",
    // Individual plans replay captured people instead of captured companies;
    // both names carry the same list so either wording works in a prompt.
    "known_people",
    "known_people_csv",
    "explored_seeds",
    "explored_seeds_csv",
    "source",
    "plan_type",
    "target_remaining",
    "current_scrape_prompt",
    "prompt_history_csv",
];

fn is_supplied(field: &str, seed_keys: &BTreeSet<String>) -> bool {
    seed_keys.contains(field) || INJECTED_VARS.contains(&field)
}

fn fields_in(tmpl: &str) -> BTreeSet<String> {
    field_regex()
        .captures_iter(tmpl)
        .map(|c| c[1].to_string())
        .collect()
}

/// Builds the output contract appended to agent-authored scrape prompts.
///
/// A free-agent prompt may rewrite the search strategy however it likes, but
/// the JSON keys it emits still feed the stored mapping templates. Stating
/// them explicitly is what keeps rewritten prompts from producing rows that
/// `Mapping::map` rejects as unmappable.
pub fn schema_contract(m: &Mapping, seed_keys: &BTreeSet<String>) -> String {
    let required: Vec<String> = m
        .key_fields()
        .into_iter()
        .filter(|f| !is_supplied(f, seed_keys))
        .collect();
    let optional: Vec<String> = m
        .row_fields(seed_keys)
        .into_iter()
        .filter(|f| !required.contains(f))
        .collect();

    let mut out = String::from(
        "\n\n=== OUTPUT CONTRACT (enforced by the pipeline — rows that break it are dropped) ===\n\
         Respond with ONLY a fenced ```json``` array of objects. Each object MUST contain:\n",
    );
    for f in &required {
        out.push_str(&format!("  {f}  (REQUIRED, non-empty — dedupe key)\n"));
    }
    if !optional.is_empty() {
        out.push_str(&format!(
            "  {}  (use \"\" if unknown, never omit)\n",
            optional.join(", ")
        ));
    }
    out.push_str("Extra keys are allowed and preserved. Do not rename these keys.\n");
    out
}

fn strip_money(s: &str) -> String {
    s.trim().trim_start_matches('$').replace(',', "")
}

fn field_regex() -> &'static Regex {
    static FIELD: OnceLock<Regex> = OnceLock::new();
    FIELD.get_or_init(|| Regex::new(r"\{\{\s*\.([A-Za-z0-9_]+)\s*\}\}").unwrap())
}

/// Renders a `{{.key}}` template against the data context. Missing keys
/// render as empty strings rather than failing the run. String values render
/// verbatim; numbers/bools render plainly; arrays/objects render as JSON;
/// null renders as "".
pub fn render_template(tmpl: &str, data: &Ctx) -> Result<String> {
    let re = field_regex();
    let mut out = String::with_capacity(tmpl.len());
    let mut last = 0;
    for m in re.captures_iter(tmpl) {
        let whole = m.get(0).unwrap();
        out.push_str(&tmpl[last..whole.start()]);
        out.push_str(&value_to_string(data.get(&m[1])));
        last = whole.end();
    }
    out.push_str(&tmpl[last..]);
    Ok(out)
}

fn value_to_string(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(other) => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_fields_and_blanks_missing() {
        let mut ctx = Ctx::new();
        ctx.insert("ein".into(), json!("12-345"));
        ctx.insert("plan_number".into(), json!(7));
        assert_eq!(
            render_template("{{.ein}}-{{ .plan_number }}{{.missing}}", &ctx).unwrap(),
            "12-345-7"
        );
    }

    #[test]
    fn mapping_cleanses_before_storing() {
        let mapping = Mapping {
            source: "s".into(),
            source_key: "{{.domain}}".into(),
            name: "{{.who}}".into(),
            title: "{{.role}}".into(),
            phone: "{{.tel}}".into(),
            ..Default::default()
        };
        let mut row = Ctx::new();
        // A zero-width space in the key, curly quote + em dash + NBSP + a
        // newline in the display fields — all typical of scraped HTML.
        row.insert("domain".into(), json!("bitso\u{200B}.com"));
        row.insert("who".into(), json!("Jose\u{0301}\u{00A0}Curi"));
        row.insert("role".into(), json!("Head of Finance \u{2014} owns\n treasury"));
        row.insert("tel".into(), json!("+52\u{00A0}55\u{200B}1234"));

        let p = mapping.map(&row, &row).unwrap();
        assert_eq!(p.source_key, "bitso.com");
        assert_eq!(p.name, "José Curi"); // accent kept, NBSP normalized
        assert_eq!(p.title, "Head of Finance - owns treasury");
        // NBSP becomes a real space; the zero-width space is removed outright
        // rather than widened into one.
        assert_eq!(p.phone, "+52 551234");
    }

    #[test]
    fn parses_money() {
        assert_eq!(strip_money(" $1,234,567 "), "1234567");
    }

    fn contract_mapping() -> Mapping {
        Mapping {
            source: "s".into(),
            source_key: "{{.company_domain}}".into(),
            name: "{{.contact_name}}".into(),
            company: "{{.company}}".into(),
            // Mixes a seed var with a row field — only the row field belongs
            // in the contract.
            location: "{{.hq_city}}, {{.country}}".into(),
            notes: "{{.known_companies_csv}}".into(),
            ..Default::default()
        }
    }

    fn seed_keys(keys: &[&str]) -> BTreeSet<String> {
        keys.iter().map(|k| k.to_string()).collect()
    }

    #[test]
    fn row_fields_exclude_seed_and_injected_vars() {
        let m = contract_mapping();
        assert_eq!(
            m.referenced_fields(),
            vec![
                "company",
                "company_domain",
                "contact_name",
                "country",
                "hq_city",
                "known_companies_csv",
            ]
        );
        // country is a seed var, known_companies_csv is injected by the loop.
        assert_eq!(
            m.row_fields(&seed_keys(&["country"])),
            vec!["company", "company_domain", "contact_name", "hq_city"]
        );
    }

    #[test]
    fn contract_marks_the_source_key_field_required() {
        let c = schema_contract(&contract_mapping(), &seed_keys(&["country"]));
        assert!(c.contains("company_domain  (REQUIRED"));
        assert!(c.contains("company, contact_name, hq_city  (use \"\" if unknown"));
        // Seed and injected vars never become scrape requirements.
        assert!(!c.contains("country"));
        assert!(!c.contains("known_companies_csv"));
    }
}
