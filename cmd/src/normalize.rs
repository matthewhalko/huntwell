//! Display-string normalization for scraped text: character cleansing, then
//! smart title-casing of ALL-CAPS values.
//!
//! Scraped names and titles arrive with whatever the source page contained —
//! non-breaking spaces, zero-width joiners left over from HTML, curly quotes,
//! em dashes, embedded newlines. Those render as boxes, gaps, or garbage
//! depending on where the CSV is opened, so [`cleanse`] folds them to plain
//! ASCII punctuation and single spaces before anything is stored.
//!
//! What cleansing deliberately does NOT touch: letters carrying diacritics.
//! `José`, `Yañez` and `Ramírez` are correctly spelled names, not encoding
//! damage, and stripping their accents would corrupt real data. Only
//! characters that are invisible, control, or purely typographic are changed.

use std::collections::HashSet;
use std::sync::OnceLock;

use unicode_normalization::UnicodeNormalization;

/// Acronyms / abbreviations preserved as-is when title-casing. Lookup is
/// case-insensitive against the uppercased form of the word.
fn preserve_caps() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        [
            // Business entity suffixes
            "LLC", "L.L.C.", "LLP", "LP", "L.P.", "INC", "INC.", "CORP", "CORP.", "CO", "CO.",
            "NA", "N.A.", "USA", "U.S.A.", "DBA", "FBO", "LTD", "LTD.", "PLC", "PA", "PC",
            "PLLC",
            // Roman numerals commonly appearing in names/orgs
            "II", "III", "IV", "VI", "VII", "VIII",
            // Professional credentials (no ® here — those tag onto already-styled text)
            "CFA", "CFP", "CPA", "MBA", "MD", "JD", "DDS", "DO", "ESQ", "PHD", "RN", "RIA",
            "AIF", "AAMS", "CIMA", "CPWA", "AWMA", "CHFC", "CLU", "CASL", "CRPC", "RICP",
            "CHSNC", "EA", "AIA", "CDFA", "ADPA", "CEPA", "CFS", "CKA", "CPFA",
            // US states + DC. Several overlap with business/credential keys above
            // ("CO" = Colorado AND Company; "MD" = Maryland AND M.D.; "PA" = Penn
            // AND legal P.A.). All overlaps want uppercase preserved, so a single
            // entry per uppercase form is correct.
            "AL", "AK", "AZ", "AR", "CT", "FL", "GA", "HI", "IL", "IA", "KS", "KY", "ME", "MI",
            "MN", "MS", "MO", "MT", "NE", "NV", "NH", "NJ", "NM", "NY", "NC", "ND", "OH", "OK",
            "OR", "RI", "SC", "SD", "TN", "TX", "UT", "VT", "VA", "WA", "WV", "WI", "WY", "DC",
            // Common org acronyms
            "IT", "HR", "IT.", "401K", "401(K)",
            // Executive / functional titles. Without these an all-caps scraped
            // title like "CFO" title-cases to the meaningless "Cfo".
            "CEO", "CFO", "COO", "CTO", "CIO", "CMO", "CRO", "CPO", "CCO", "CDO", "CISO",
            "VP", "SVP", "EVP", "AVP", "GM", "FP", "PM",
            // Payments / fintech, incl. the Mexican regulatory alphabet these
            // LatAm sources are full of (IFPE, SOFIPO, CNBV, SPEI, S.A.P.I.).
            "PSP", "IFPE", "SOFIPO", "SOFOM", "SAPI", "S.A.P.I.", "CNBV", "CONDUSEF", "SPEI",
            "CLABE", "SA", "S.A.", "CV", "C.V.", "SAB", "API", "APIS", "ACH", "SWIFT", "FX",
            "ALM", "AML", "KYC", "KYB", "B2B", "B2C", "P2P", "USDC", "USDT", "BTC", "ETH",
            "ETF", "IRA", "IPO", "AUM", "ESG", "ROI", "GTM", "HQ", "UX", "CX",
            // Regions
            "LATAM", "CDMX", "MX", "UK", "EU", "EMEA", "APAC",
        ]
        .into_iter()
        .collect()
    })
}

/// Cleanses a display string: composes accents, drops invisible and control
/// characters, folds typographic punctuation to ASCII, and collapses every
/// run of whitespace (including embedded newlines) to a single space.
///
/// Accented letters survive untouched — see the module docs.
pub fn cleanse(s: &str) -> String {
    scrub(s, true)
}

/// The lossless half of [`cleanse`], for values whose exact bytes matter:
/// invisible characters, control characters and stray whitespace go, but no
/// punctuation is rewritten. Used for SourceKey, where a zero-width space
/// smuggled in from a web page would otherwise create a phantom duplicate
/// that never dedupes, but where folding a dash would silently split one
/// prospect into two.
pub fn cleanse_key(s: &str) -> String {
    scrub(s, false)
}

/// [`cleanse`] for values where line breaks are content, not noise — drafted
/// outreach messages keep their paragraphs. Each line is cleansed on its own;
/// runs of blank lines collapse to a single blank line.
pub fn cleanse_multiline(s: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut blank_pending = false;
    for line in s.lines() {
        let clean = scrub(line, true);
        if clean.is_empty() {
            blank_pending = !out.is_empty();
            continue;
        }
        if blank_pending {
            out.push(String::new());
            blank_pending = false;
        }
        out.push(clean);
    }
    out.join("\n")
}

fn scrub(s: &str, fold_punctuation: bool) -> String {
    // NFC first: web sources mix composed "é" (U+00E9) with decomposed
    // "e" + U+0301. They look identical but compare and sort differently, so
    // dedupe and ORDER BY misbehave unless one form wins.
    let mut out = String::with_capacity(s.len());
    let mut at_space = false;
    for ch in s.nfc() {
        // Any whitespace — tab, newline, NBSP, thin space, ideographic space —
        // becomes one ordinary space.
        if ch.is_whitespace() {
            if !out.is_empty() && !at_space {
                out.push(' ');
                at_space = true;
            }
            continue;
        }
        let code = ch as u32;
        let replacement: &str = match ch {
            // Invisible: soft hyphen, zero-width space/joiners, bidi controls,
            // word joiner, BOM. All render as nothing or as a box.
            '\u{00AD}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}' | '\u{FEFF}' => continue,
            // U+FFFD means the text was already mis-decoded upstream; keeping
            // it just propagates a visible replacement diamond.
            '\u{FFFD}' => continue,
            // C0/C1 control characters (non-whitespace ones).
            _ if code < 0x20 || (0x7F..0xA0).contains(&code) => continue,
            _ if !fold_punctuation => {
                out.push(ch);
                at_space = false;
                continue;
            }
            // Curly quotes and primes -> ASCII quotes.
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{2032}' => "'",
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{2033}' => "\"",
            // Hyphens, en/em dashes, minus sign, fullwidth hyphen -> "-".
            '\u{2010}'..='\u{2015}' | '\u{2212}' | '\u{FE58}' | '\u{FE63}' | '\u{FF0D}' => "-",
            '\u{2026}' => "...",
            '\u{2022}' => "-",
            _ => {
                out.push(ch);
                at_space = false;
                continue;
            }
        };
        out.push_str(replacement);
        at_space = false;
    }
    // A trailing space can only be the collapsed run at the end.
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Cleanses, then normalizes display strings. Rules:
///   - If the string already contains any lowercase letter, assume it's
///     already styled correctly and return it cleansed but un-recased.
///   - Otherwise apply title case word-by-word, preserving entries in
///     `preserve_caps` verbatim (LLC, CFA, NV, IV, CFO, IFPE, etc.).
pub fn smart_title_case(s: &str) -> String {
    let s = cleanse(s);
    if s.is_empty() {
        return String::new();
    }
    if s.chars().any(|c| c.is_lowercase()) {
        return s; // already styled — leave the casing alone
    }
    title_case_words(&s)
}

/// Particles that stay lowercase inside a title-cased phrase — English and
/// Spanish, since these sources produce both ("Head of Treasury", "Director
/// de ALM"). Never applied to the first word.
fn small_words() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        [
            "OF", "THE", "AND", "FOR", "A", "AN", "AT", "BY", "IN", "ON", "TO", "OR", "VS",
            "DE", "DEL", "LA", "LAS", "EL", "LOS", "Y", "EN", "PARA",
        ]
        .into_iter()
        .collect()
    })
}

fn title_case_words(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut word: Vec<char> = Vec::with_capacity(16);
    let mut is_first_word = true;

    let mut flush = |word: &mut Vec<char>, out: &mut String| {
        if word.is_empty() {
            return;
        }
        let w: String = word.iter().collect();
        let upper = w.to_uppercase();
        let first = is_first_word;
        is_first_word = false;
        if preserve_caps().contains(upper.as_str()) {
            out.push_str(&upper);
        } else if !first && small_words().contains(upper.as_str()) {
            out.push_str(&w.to_lowercase());
        } else {
            for (i, c) in word.iter().enumerate() {
                if i == 0 {
                    out.extend(c.to_uppercase());
                } else if i >= 2 && word[i - 1] == '\'' {
                    // Capitalize after apostrophe in surnames like O'BRIEN, D'ANGELO
                    out.extend(c.to_uppercase());
                } else {
                    out.extend(c.to_lowercase());
                }
            }
        }
        word.clear();
    };

    for r in s.chars() {
        // Group letters, digits, periods, and apostrophes into one "word"
        // so "L.L.C." and "O'BRIEN" stay together.
        if r.is_alphanumeric() || r == '\'' || r == '.' {
            word.push(r);
        } else {
            flush(&mut word, &mut out);
            out.push(r);
        }
    }
    flush(&mut word, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accents_are_data_not_damage() {
        // Real names from Mexican sources. Cleansing must not touch them.
        for name in [
            "José Luis Curi Lepe",
            "Jimena Yañez Rangel",
            "Abraham Cobos Ramírez",
            "Alejandra González Quintanar",
        ] {
            assert_eq!(cleanse(name), name, "cleanse mangled {name:?}");
            assert_eq!(smart_title_case(name), name);
        }
    }

    #[test]
    fn decomposed_accents_are_composed() {
        let decomposed = "Jose\u{0301}"; // "e" + combining acute
        assert_eq!(decomposed.chars().count(), 5);
        let got = cleanse(decomposed);
        assert_eq!(got, "José");
        assert_eq!(got.chars().count(), 4);
    }

    #[test]
    fn invisible_characters_are_dropped() {
        assert_eq!(cleanse("Ali\u{200B}ce"), "Alice"); // zero-width space
        assert_eq!(cleanse("\u{FEFF}Bob"), "Bob"); // BOM
        assert_eq!(cleanse("Ma\u{00AD}ria"), "Maria"); // soft hyphen
        assert_eq!(cleanse("A\u{202E}B"), "AB"); // bidi override
        assert_eq!(cleanse("Jos\u{FFFD}"), "Jos"); // already-broken decode
        assert_eq!(cleanse("Bad\u{0007}Bell"), "BadBell"); // control char
    }

    #[test]
    fn typographic_punctuation_folds_to_ascii() {
        assert_eq!(
            cleanse("Head of Finance \u{2014} Conekta"),
            "Head of Finance - Conekta"
        );
        assert_eq!(cleanse("no \u{201C}Head of Treasury\u{201D}"), "no \"Head of Treasury\"");
        assert_eq!(cleanse("O\u{2019}Brien"), "O'Brien");
        assert_eq!(cleanse("Director\u{2026}"), "Director...");
        assert_eq!(cleanse("2020\u{2013}2024"), "2020-2024");
    }

    #[test]
    fn whitespace_collapses_to_single_spaces() {
        assert_eq!(cleanse("  Head   of\tTreasury \n & Risk  "), "Head of Treasury & Risk");
        assert_eq!(cleanse("Nvio\u{00A0}Pagos"), "Nvio Pagos"); // non-breaking space
        assert_eq!(cleanse("Multi\nline\ntitle"), "Multi line title");
        assert_eq!(cleanse("   "), "");
    }

    #[test]
    fn key_cleansing_is_lossless_on_punctuation() {
        // Invisible junk goes...
        assert_eq!(cleanse_key("bitso\u{200B}.com"), "bitso.com");
        assert_eq!(cleanse_key(" bitso.com\n"), "bitso.com");
        // ...but a dash is never rewritten: that would split one prospect in two.
        assert_eq!(cleanse_key("felix\u{2013}pago.com"), "felix\u{2013}pago.com");
        assert_eq!(cleanse_key("O\u{2019}Brien-co.com"), "O\u{2019}Brien-co.com");
    }

    #[test]
    fn business_acronyms_survive_title_casing() {
        // The bug that stored "PSP" as "Psp".
        assert_eq!(smart_title_case("PSP"), "PSP");
        assert_eq!(smart_title_case("CFO"), "CFO");
        assert_eq!(smart_title_case("HEAD OF TREASURY, CFO"), "Head of Treasury, CFO");
        assert_eq!(smart_title_case("NVIO PAGOS MEXICO, IFPE"), "Nvio Pagos Mexico, IFPE");
        assert_eq!(smart_title_case("BITSO SA DE CV"), "Bitso SA de CV");
    }

    #[test]
    fn particles_stay_lowercase_except_first() {
        assert_eq!(smart_title_case("HEAD OF GROWTH"), "Head of Growth");
        assert_eq!(smart_title_case("DIRECTOR DE ALM Y RIESGOS"), "Director de ALM y Riesgos");
        assert_eq!(smart_title_case("BANCO DE LA NACION"), "Banco de la Nacion");
        // A particle leading the string keeps its capital.
        assert_eq!(smart_title_case("THE HOME DEPOT"), "The Home Depot");
        assert_eq!(smart_title_case("DE LA CRUZ HOLDINGS"), "De la Cruz Holdings");
    }

    #[test]
    fn test_smart_title_case() {
        let cases = [
            ("COTTONWOOD MANAGEMENT LLC", "Cottonwood Management LLC"),
            ("CLARKE BROADCASTING CORP.", "Clarke Broadcasting CORP."),
            ("INCLINE LAW GROUP, LLP", "Incline Law Group, LLP"),
            ("INCLINE VILLAGE, NV", "Incline Village, NV"),
            ("It's Today Media LLC", "It's Today Media LLC"), // mixed-case, leave alone
            ("DANIEL FYLSTRA", "Daniel Fylstra"),
            ("H. RANDOLPH HOLDER, JR.", "H. Randolph Holder, Jr."),
            ("401(K) PLAN SPONSOR", "401(K) Plan Sponsor"),
            ("O'BRIEN & SONS, INC.", "O'Brien & Sons, INC."),
            ("PRO 10-4 HOLDINGS, INC.", "Pro 10-4 Holdings, INC."),
            ("", ""),
            ("  COMPANY NAME  ", "Company Name"),
        ];
        for (input, want) in cases {
            let got = smart_title_case(input);
            assert_eq!(got, want, "smart_title_case({input:?})");
        }
    }
}
