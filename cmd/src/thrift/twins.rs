//! The same listing found on two sites is enriched once.
//!
//! A car on cars.com is usually on cargurus too. Both rows come back from one
//! scrape, both are new, and each would get its own enrich call — the most
//! expensive step — to learn the same things twice.
//!
//! Two rows are twins only when all of this holds, so that a near-miss is never
//! mistaken for a match:
//!   - they are on **different sites** (one site listing two similar cars means
//!     two cars);
//!   - their titles are nearly the same text (character trigrams, ≥ 0.8);
//!   - they share at least one number column, and **every** number column they
//!     share is equal — same price, same mileage, same year.
//!
//! Plain arithmetic on the CPU: no model, nothing to download, and the rule is
//! the same for every kind of row.

use std::collections::{BTreeMap, HashSet};

use serde_json::Value;

type Row = BTreeMap<String, Value>;

const ALIKE: f64 = 0.8;

struct Known {
    key: String,
    host: String,
    grams: HashSet<String>,
    numbers: BTreeMap<String, f64>,
}

/// Rows seen so far in this pass.
#[derive(Default)]
pub struct Twins {
    known: Vec<Known>,
}

impl Twins {
    /// The key of an earlier row this one duplicates, if any. Otherwise the row
    /// is remembered and `None` returned.
    pub fn twin_or_remember(&mut self, key: &str, title: &str, url: &str, row: &Row) -> Option<String> {
        if !super::on("twins") {
            return None;
        }
        let me = Known {
            key: key.to_string(),
            host: super::page::host_of(url).unwrap_or_default(),
            grams: trigrams(title),
            numbers: row.iter().filter_map(|(k, v)| super::filters::number(v).map(|n| (k.clone(), n))).collect(),
        };
        let twin = self.known.iter().find(|other| is_twin(&me, other)).map(|o| o.key.clone());
        if twin.is_none() {
            self.known.push(me);
        }
        twin
    }
}

fn is_twin(a: &Known, b: &Known) -> bool {
    if a.host.is_empty() || b.host.is_empty() || a.host == b.host {
        return false;
    }
    let shared: Vec<&String> = a.numbers.keys().filter(|k| b.numbers.contains_key(*k)).collect();
    if shared.is_empty() || shared.iter().any(|k| a.numbers[*k] != b.numbers[*k]) {
        return false;
    }
    jaccard(&a.grams, &b.grams) >= ALIKE
}

fn trigrams(s: &str) -> HashSet<String> {
    let flat: Vec<char> = s.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
    flat.windows(3).map(|w| w.iter().collect()).collect()
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    a.intersection(b).count() as f64 / a.union(b).count() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn car(price: i64, miles: i64) -> Row {
        Row::from([("price".to_string(), Value::from(price)), ("mileage".to_string(), Value::from(format!("{miles} mi")))])
    }

    #[test]
    fn the_same_car_on_another_site_is_a_twin() {
        let mut t = Twins::default();
        assert_eq!(t.twin_or_remember("A", "2022 Subaru Crosstrek Limited", "https://www.cars.test/v/1", &car(27995, 29940)), None);
        let twin = t.twin_or_remember("B", "2022 Subaru Crosstrek Limited AWD", "https://cargurus.test/l/9", &car(27995, 29940));
        assert_eq!(twin.as_deref(), Some("A"));
    }

    #[test]
    fn a_near_miss_is_two_cars() {
        let mut t = Twins::default();
        t.twin_or_remember("A", "2022 Subaru Crosstrek Limited", "https://cars.test/v/1", &car(27995, 29940));
        // Same site: a dealer with two alike cars has two cars.
        assert_eq!(t.twin_or_remember("B", "2022 Subaru Crosstrek Limited", "https://cars.test/v/2", &car(27995, 29940)), None);
        // Other site, different mileage.
        assert_eq!(t.twin_or_remember("C", "2022 Subaru Crosstrek Limited", "https://other.test/1", &car(27995, 31000)), None);
        // Other site, a different car at the same price and mileage.
        assert_eq!(t.twin_or_remember("D", "2019 Toyota RAV4 XLE", "https://third.test/1", &car(27995, 29940)), None);
        // Other site, same title, but no number in common to confirm it.
        assert_eq!(t.twin_or_remember("E", "2022 Subaru Crosstrek Limited", "https://fourth.test/1", &Row::new()), None);
    }
}
