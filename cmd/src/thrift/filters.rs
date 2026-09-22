//! Rows outside a column's stated limits, dropped before they cost anything.
//!
//! A brief that says "under $50,000" gives the price column `max: 50000` when
//! the plan is drafted. A scraped row priced at $61,000 will never be wanted,
//! and enriching it is the most expensive thing a run does per row — so it is
//! dropped first. Only a value that reads unambiguously as a number can fail a
//! limit: "Call for price", a blank, or a range is not outside anything, and
//! the row stays.

use serde_json::Value;

use crate::artifact::FieldSpec;

type Row = std::collections::BTreeMap<String, Value>;

/// Why this row is outside the plan's limits, or `None` if it is not — or if
/// it cannot be told.
pub fn outside(row: &Row, schema: &[FieldSpec]) -> Option<String> {
    if !super::on("filters") {
        return None;
    }
    for f in schema {
        if f.min.is_none() && f.max.is_none() {
            continue;
        }
        let Some(n) = row.get(&f.key).and_then(number) else { continue };
        let label = if f.label.trim().is_empty() { &f.key } else { &f.label };
        if let Some(max) = f.max {
            if n > max {
                return Some(format!("{label} {n} is over the plan's limit of {max}"));
            }
        }
        if let Some(min) = f.min {
            if n < min {
                return Some(format!("{label} {n} is under the plan's limit of {min}"));
            }
        }
    }
    None
}

/// A value as a number, only if that is plainly what it is: digits with
/// optional currency sign, thousands separators, decimals and a trailing unit.
pub fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let t = s.trim().trim_start_matches(['$', '€', '£', '~']).trim();
            let end = t.find(|c: char| !(c.is_ascii_digit() || c == ',' || c == '.')).unwrap_or(t.len());
            let (digits, unit) = t.split_at(end);
            // Whatever follows must be a bare unit ("mi", "miles", "USD"), not
            // more numbers: "20,000 - 30,000" is a range, and says nothing.
            if digits.is_empty() || unit.chars().any(|c| c.is_ascii_digit()) || unit.trim().len() > 8 {
                return None;
            }
            digits.replace(',', "").parse().ok()
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn price(max: Option<f64>, min: Option<f64>) -> Vec<FieldSpec> {
        vec![FieldSpec { key: "price".into(), label: "Price".into(), ftype: "money".into(), role: String::new(), min, max }]
    }
    fn row(v: Value) -> Row {
        Row::from([("price".to_string(), v)])
    }

    #[test]
    fn a_row_over_the_limit_goes_and_says_why() {
        let why = outside(&row(Value::from("$61,250")), &price(Some(50_000.0), None)).expect("over");
        assert!(why.contains("Price") && why.contains("61250") && why.contains("50000"), "{why}");
        assert!(outside(&row(Value::from(49_999)), &price(Some(50_000.0), None)).is_none());
        assert!(outside(&row(Value::from("900 mi")), &price(None, Some(1_000.0))).is_some());
    }

    #[test]
    fn what_is_not_plainly_a_number_is_never_outside() {
        for v in ["Call for price", "", "20,000 - 30,000", "from 61000", "61k"] {
            assert!(outside(&row(Value::from(v)), &price(Some(50_000.0), None)).is_none(), "{v:?}");
        }
        assert!(outside(&Row::new(), &price(Some(1.0), None)).is_none(), "a missing value is not a failing one");
    }

    #[test]
    fn a_column_without_limits_filters_nothing() {
        assert!(outside(&row(Value::from(9e9)), &price(None, None)).is_none());
    }
}
