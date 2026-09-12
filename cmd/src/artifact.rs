//! Custom-artifact collection: the schema-flexible counterpart to `prospect.rs`.
//!
//! A `Kind = 'artifacts'` plan carries a `FieldsSchemaJson` — a list of column
//! specs `{key, label, type, role}`. The scrape agent returns a JSON array whose
//! objects use those `key`s directly (no template layer — the schema key *is*
//! the JSON key), and each row is coerced by `type` and stored as an `Artifact`.
//!
//! `role` picks the three special fields: `key` (the dedupe identity), `title`
//! (a human label) and `url` (the source link). Everything else is kept in
//! `FieldsJson`; the whole raw row is kept in `MetaData`.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::normalize::{cleanse, cleanse_key, cleanse_multiline};
use crate::prospect::Ctx;

/// One column of a custom-artifact plan. Serializable because the artifacts
/// endpoint sends the columns along with the rows they describe — a plan's own
/// definition never leaves the server.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FieldSpec {
    pub key: String,
    #[serde(default)]
    pub label: String,
    /// text | longtext | number | money | url | date
    #[serde(default, rename = "type")]
    pub ftype: String,
    /// "" | key | title | url
    #[serde(default)]
    pub role: String,
}

impl FieldSpec {
    fn is(&self, role: &str) -> bool {
        self.role.eq_ignore_ascii_case(role)
    }
    fn typed(&self, t: &str) -> bool {
        self.ftype.eq_ignore_ascii_case(t)
    }
}

/// Parse a plan's `FieldsSchemaJson` into column specs. Tolerant: a malformed or
/// empty schema yields no columns (the caller treats that as an error).
pub fn parse_schema(json: &str) -> Vec<FieldSpec> {
    if json.trim().is_empty() {
        return Vec::new();
    }
    serde_json::from_str::<Vec<FieldSpec>>(json)
        .unwrap_or_default()
        .into_iter()
        .filter(|f| !f.key.trim().is_empty())
        .collect()
}

/// The write model — one stored artifact. Mirrors `prospect::Prospect`.
#[derive(Debug, Default, Clone)]
pub struct Artifact {
    pub source_key: String,
    pub title: String,
    pub url: String,
    /// JSON object of the schema fields → coerced values.
    pub fields_json: String,
    /// The raw scraped row, as JSON.
    pub metadata: String,
}

/// Projects scraped rows into `Artifact`s per a plan's schema.
pub struct ArtifactMapping {
    schema: Vec<FieldSpec>,
    key_field: String,
    title_field: String,
    url_field: String,
}

impl ArtifactMapping {
    pub fn new(schema: Vec<FieldSpec>) -> Self {
        // dedupe key: role=key, else role=url, else a url-typed field, else first.
        let key_field = pick(&schema, |f| f.is("key"))
            .or_else(|| pick(&schema, |f| f.is("url")))
            .or_else(|| pick(&schema, |f| f.typed("url")))
            .or_else(|| schema.first().map(|f| f.key.clone()))
            .unwrap_or_default();
        // title: role=title, else first text field, else first.
        let title_field = pick(&schema, |f| f.is("title"))
            .or_else(|| pick(&schema, |f| f.typed("text")))
            .or_else(|| schema.first().map(|f| f.key.clone()))
            .unwrap_or_default();
        // url: role=url, else first url-typed field.
        let url_field = pick(&schema, |f| f.is("url"))
            .or_else(|| pick(&schema, |f| f.typed("url")))
            .unwrap_or_default();
        Self { schema, key_field, title_field, url_field }
    }

    pub fn has_schema(&self) -> bool {
        !self.schema.is_empty()
    }

    pub fn schema(&self) -> &[FieldSpec] {
        &self.schema
    }

    pub fn key_field(&self) -> &str {
        &self.key_field
    }

    /// Puts the scrape-time identity back on the row so a later remap cannot
    /// change which artifact this is. Enrichment that copies another listing's
    /// VIN into the key field would otherwise collapse ten cars into one write.
    pub fn lock_key(&self, row: &mut Ctx, original_key: &str) {
        if self.key_field.is_empty() || original_key.is_empty() {
            return;
        }
        row.insert(self.key_field.clone(), Value::String(original_key.to_string()));
    }

    /// Coerce one scraped row into an artifact. Errors (unmappable) when the
    /// dedupe key renders empty — the same rule as prospects.
    pub fn map(&self, row: &Ctx) -> Result<Artifact> {
        let mut fields = Map::new();
        for f in &self.schema {
            let raw = row.get(&f.key).map(val_str).unwrap_or_default();
            let v = if f.typed("number") || f.typed("money") {
                match parse_number(&raw) {
                    Some(n) => serde_json::Number::from_f64(n).map(Value::Number).unwrap_or(Value::String(raw)),
                    None => Value::String(cleanse(&raw)),
                }
            } else if f.typed("longtext") {
                Value::String(cleanse_multiline(&raw))
            } else {
                Value::String(cleanse(&raw))
            };
            fields.insert(f.key.clone(), v);
        }

        let source_key = cleanse_key(&row.get(&self.key_field).map(val_str).unwrap_or_default());
        if source_key.is_empty() {
            bail!("artifact key field {:?} rendered empty for row: {:?}", self.key_field, row);
        }
        let title = row.get(&self.title_field).map(val_str).unwrap_or_default();
        let url = cleanse(&row.get(&self.url_field).map(val_str).unwrap_or_default());

        Ok(Artifact {
            source_key,
            title: cleanse(&title),
            url,
            fields_json: serde_json::to_string(&fields).unwrap_or_else(|_| "{}".into()),
            metadata: serde_json::to_string(row).unwrap_or_else(|_| "{}".into()),
        })
    }
}

/// The output contract appended to an artifact plan's scrape prompt, so the
/// agent returns exactly the schema's keys. Mirrors `prospect::schema_contract`.
pub fn schema_contract(schema: &[FieldSpec]) -> String {
    if schema.is_empty() {
        return String::new();
    }
    let mapping = ArtifactMapping::new(schema.to_vec());
    let mut out = String::from(
        "\n\nRespond with ONLY a fenced ```json``` array (no prose). Each object uses exactly these keys:\n",
    );
    for f in schema {
        let mut notes: Vec<String> = Vec::new();
        if f.key == mapping.key_field {
            notes.push("REQUIRED, unique per item".into());
        }
        if !f.ftype.is_empty() {
            notes.push(f.ftype.clone());
        }
        let label = if f.label.is_empty() { &f.key } else { &f.label };
        if notes.is_empty() {
            out.push_str(&format!("- {} ({})\n", f.key, label));
        } else {
            out.push_str(&format!("- {} ({}) [{}]\n", f.key, label, notes.join(", ")));
        }
    }
    out.push_str("Omit an item entirely rather than inventing a value you did not find.\n");
    out
}

/// Appended to one-item enrich calls. The scrape already named this row; the
/// enrich agent is only allowed to open *this* URL and fill blanks.
pub fn enrich_identity_note(mapping: &ArtifactMapping, a: &Artifact) -> String {
    let url = if a.url.trim().is_empty() {
        "this item's own source URL".to_string()
    } else {
        a.url.clone()
    };
    format!(
        "\n\nThis call is for ONE item. Open {url} and read that page only — \
         do not reuse a tab that belongs to a different item.\n\
         Do not change {}: keep it exactly {:?}. Changing the id merges this \
         item into another row.\n",
        mapping.key_field(),
        a.source_key
    )
}

fn pick(schema: &[FieldSpec], pred: impl Fn(&FieldSpec) -> bool) -> Option<String> {
    schema.iter().find(|f| pred(f)).map(|f| f.key.clone())
}

/// A scraped value as a display string (matches how render_template flattens).
fn val_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_string(),
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

fn parse_number(s: &str) -> Option<f64> {
    let cleaned: String = s.chars().filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
    let cleaned = cleaned.trim_matches('.');
    if cleaned.is_empty() {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(pairs: &[(&str, Value)]) -> Ctx {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    #[test]
    fn maps_schema_fields_and_types() {
        let schema = parse_schema(
            r#"[{"key":"make","label":"Make","type":"text"},
                {"key":"price","label":"Price","type":"money"},
                {"key":"url","label":"Listing","type":"url","role":"url"},
                {"key":"vin","label":"VIN","type":"text","role":"key"}]"#,
        );
        let m = ArtifactMapping::new(schema);
        let a = m
            .map(&row(&[
                ("make", json!("Subaru")),
                ("price", json!("$14,900")),
                ("url", json!("https://x.com/1")),
                ("vin", json!("JF1-ABC")),
            ]))
            .unwrap();
        assert_eq!(a.source_key, "JF1-ABC"); // cleanse_key is lossless (keeps case)
        assert_eq!(a.url, "https://x.com/1");
        let f: Value = serde_json::from_str(&a.fields_json).unwrap();
        assert_eq!(f["make"], json!("Subaru"));
        assert_eq!(f["price"], json!(14900.0));
    }

    #[test]
    fn enrich_cannot_rename_the_row() {
        let schema = parse_schema(
            r#"[{"key":"listing_id","role":"key"},{"key":"title","role":"title"},{"key":"listing_url","type":"url","role":"url"}]"#,
        );
        let m = ArtifactMapping::new(schema);
        let original = "https://craigslist.org/reno-legacy";
        let mut r = row(&[
            ("listing_id", json!(original)),
            ("title", json!("2024 Subaru Legacy")),
            ("listing_url", json!(original)),
        ]);
        // The enrich agent copied another car's VIN onto this row.
        r.insert("listing_id".into(), json!("JF2GTAEC8K8214075"));
        m.lock_key(&mut r, original);
        let a = m.map(&r).unwrap();
        assert_eq!(a.source_key, original);
    }

    #[test]
    fn empty_key_is_unmappable() {
        let schema = parse_schema(r#"[{"key":"url","type":"url","role":"key"}]"#);
        let m = ArtifactMapping::new(schema);
        assert!(m.map(&row(&[("url", json!(""))])).is_err());
    }

    #[test]
    fn key_defaults_to_url_then_first() {
        let schema = parse_schema(r#"[{"key":"title","type":"text"},{"key":"link","type":"url"}]"#);
        let m = ArtifactMapping::new(schema);
        let a = m.map(&row(&[("title", json!("A")), ("link", json!("http://a"))])).unwrap();
        assert_eq!(a.source_key, "http://a"); // url-typed field wins as key
    }
}

// ---------------------------------------------------------------------------
// Columns a person asked for
// ---------------------------------------------------------------------------

/// One column as typed in the app: a name, and what to put in it.
///
/// The prompt is not stored on the column — it is folded into the ScrapePrompt
/// when the plan is drafted, because that is the thing the scraping agent
/// actually reads. Keeping it here would mean two places to edit and one of
/// them silently ignored.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ColumnRequest {
    pub name: String,
    #[serde(default)]
    pub prompt: String,
}

/// `Column Name` -> `column_name`, safe as a JSON key and a CSV header.
fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            if gap && !out.is_empty() {
                out.push('_');
            }
            gap = false;
            out.extend(c.to_lowercase());
        } else {
            gap = true;
        }
    }
    // A key has to start with a letter to be pleasant in a spreadsheet header.
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, 'c');
    }
    out
}

/// The column type, guessed from its name.
///
/// Guessing beats asking: a fourth input per column ("is this a number?") buys
/// a little sorting behaviour at the cost of the feature feeling like a form.
/// A wrong guess degrades to text, which is what it would have been anyway.
fn guess_type(name: &str) -> &'static str {
    let n = name.to_ascii_lowercase();
    let has = |ws: &[&str]| ws.iter().any(|w| n.contains(w));
    if has(&["url", "link", "listing", "website", "page"]) {
        "url"
    } else if has(&["price", "cost", "salary", "rent", "amount", "value", "fee", "revenue"]) {
        "money"
    } else if has(&["date", "posted", "published", "founded", "deadline", "closes"]) {
        "date"
    } else if has(&["year", "count", "miles", "mileage", "km", "size", "beds", "baths", "employees", "rating", "number", "qty"]) {
        "number"
    } else if has(&["description", "summary", "notes", "details", "about"]) {
        "longtext"
    } else {
        "text"
    }
}

/// Turns the columns someone typed into a schema the pipeline can run.
///
/// Every artifact plan needs three roles filled — a stable key, a human title,
/// and a link back to where the row came from. People do not think in roles, so
/// this assigns them: the first url-ish column becomes the link, the first
/// text column becomes the title, and the link doubles as the key (a source URL
/// is unique per item almost by definition). Anything missing is added, so a
/// plan built from one column still runs.
pub fn columns_to_schema(cols: &[ColumnRequest]) -> Vec<FieldSpec> {
    let mut out: Vec<FieldSpec> = Vec::new();
    for c in cols {
        let key = slug(&c.name);
        if key.is_empty() || out.iter().any(|f| f.key == key) {
            continue;
        }
        out.push(FieldSpec { key, label: c.name.trim().to_string(), ftype: guess_type(&c.name).into(), role: String::new() });
    }
    if out.is_empty() {
        return out;
    }
    // The link back to the source: an existing url column, or one appended.
    // `ArtifactMapping::new` takes the dedupe key from the url role when no
    // explicit key column exists, so this fills two roles at once.
    let url_key = match out.iter().find(|f| f.typed("url")) {
        Some(f) => f.key.clone(),
        None => {
            out.push(FieldSpec { key: "url".into(), label: "Link".into(), ftype: "url".into(), role: String::new() });
            "url".into()
        }
    };
    // The title: the first wordy column that is not the link.
    let title_key = out
        .iter()
        .find(|f| f.key != url_key && (f.typed("text") || f.typed("longtext")))
        .map(|f| f.key.clone());
    for f in out.iter_mut() {
        if f.key == url_key {
            f.role = "url".into();
        } else if Some(&f.key) == title_key.as_ref() {
            f.role = "title".into();
        }
    }
    out
}

#[cfg(test)]
mod column_tests {
    use super::*;

    fn cols(names: &[&str]) -> Vec<ColumnRequest> {
        names.iter().map(|n| ColumnRequest { name: (*n).into(), prompt: String::new() }).collect()
    }

    #[test]
    fn names_become_keys_and_types() {
        let s = columns_to_schema(&cols(["Make", "Model Year", "Asking price", "Listing URL"].as_slice()));
        let keys: Vec<_> = s.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(keys, ["make", "model_year", "asking_price", "listing_url"]);
        assert_eq!(s[1].ftype, "number", "a year sorts as a number");
        assert_eq!(s[2].ftype, "money");
        assert_eq!(s[3].ftype, "url");
    }

    #[test]
    fn the_roles_a_run_needs_are_filled_in() {
        // Nobody types "role"; the pipeline still needs a link and a title.
        let s = columns_to_schema(&cols(["Make", "Asking price"].as_slice()));
        let m = ArtifactMapping::new(s.clone());
        assert_eq!(m.url_field, "url", "a link column is added when none was asked for");
        assert_eq!(m.title_field, "make");
        assert_eq!(m.key_field, "url", "dedupe falls back to the link");
        assert!(s.iter().any(|f| f.key == "url" && f.label == "Link"));
    }

    #[test]
    fn a_url_column_someone_typed_is_used_as_the_link() {
        let s = columns_to_schema(&cols(["Job title", "Apply link"].as_slice()));
        let m = ArtifactMapping::new(s);
        assert_eq!(m.url_field, "apply_link");
        assert_eq!(m.title_field, "job_title");
    }

    #[test]
    fn junk_and_duplicates_are_dropped_not_stored() {
        let s = columns_to_schema(&cols(["Price", "  ", "price", "!!!", "2026 rank"].as_slice()));
        let keys: Vec<_> = s.iter().map(|f| f.key.as_str()).collect();
        assert!(keys.contains(&"price"));
        assert_eq!(keys.iter().filter(|k| **k == "price").count(), 1);
        assert!(keys.contains(&"c2026_rank"), "a key may not start with a digit: {keys:?}");
    }

    #[test]
    fn no_columns_means_no_schema() {
        assert!(columns_to_schema(&[]).is_empty());
        assert!(columns_to_schema(&cols(["", " "].as_slice())).is_empty());
    }
}
