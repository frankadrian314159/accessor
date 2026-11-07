//! Collations (PostgreSQL mode): how text sorts and compares when a column or an expression names one.
//!
//! A collation turns a text into a *sort key*: bytes that compare, with an ordinary bytewise comparison, the way
//! the collation orders the texts. The binder wraps both sides of a comparison, and the keys of `ORDER BY`, in
//! `__COLLKEY(collation, value)`; nothing else in the engine needs to know about collations.
//!
//! Three kinds exist:
//!
//! * `C`, `POSIX`, `ucs_basic` (and `C.utf8`): the bytes of the text, so `B` sorts before `a`.
//! * the ICU kind (`und-x-icu`, `en-x-icu`, `sv-x-icu`, `unicode`, anything made with `CREATE COLLATION ... provider = icu`):
//!   a small version of the Unicode Collation Algorithm. Texts are compared on three levels in turn: the base letters
//!   (case and accents ignored; spaces and punctuation before digits before letters), then the accents, then the case
//!   (lower before upper). A locale can change the letters (`sv`: `å ä ö` after `z`; `es`: `ñ` after `n`), the strength
//!   (`-u-ks-level1`, `-u-ks-level2`) and read runs of digits as numbers (`-u-kn-true`).
//! * the default collation: no key at all, the comparison the engine always had.
//!
//! A *deterministic* collation still tells apart texts that are equal at every level (their bytes decide); a
//! non-deterministic one (`deterministic = false`) calls them equal, which is how a case-insensitive collation is made.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::error::{Error, Result};

#[derive(Clone, Debug, PartialEq)]
pub struct Collation {
    pub name: String,
    pub locale: String,
    pub binary: bool,
    pub deterministic: bool,
    /// 1 base letters, 2 and accents, 3 and case
    pub strength: u8,
    pub numeric: bool,
    pub upper_first: bool,
}

thread_local! {
    static USER: RefCell<HashMap<String, Collation>> = RefCell::new(HashMap::new());
}

/// Makes the collations of the catalog (`name`, JSON) the ones in force.
pub fn sync(defined: &[(String, String)]) {
    USER.with(|u| {
        let mut u = u.borrow_mut();
        u.clear();
        for (name, json) in defined {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(json) {
                let c = from_options(
                    name,
                    v.get("locale").and_then(|x| x.as_str()).unwrap_or("und"),
                    v.get("deterministic").and_then(|x| x.as_bool()).unwrap_or(true),
                    v.get("provider").and_then(|x| x.as_str()).unwrap_or("icu"),
                );
                u.insert(name.to_ascii_lowercase(), c);
            }
        }
    });
}

/// Is any collation of the catalog loaded?
pub fn any_defined() -> bool {
    USER.with(|u| !u.borrow().is_empty())
}

fn from_options(name: &str, locale: &str, deterministic: bool, provider: &str) -> Collation {
    let lower = locale.to_ascii_lowercase();
    let binary = provider == "libc" && matches!(lower.as_str(), "c" | "posix" | "c.utf8" | "c.utf-8" | "pg_c_utf8");
    let mut c = Collation { name: name.to_string(), locale: locale.to_string(), binary, deterministic, strength: 3, numeric: false, upper_first: false };
    // `-u-ks-level2-kn-true`, `@colStrength=secondary;colNumericOrdering=yes`
    let words: Vec<&str> = lower.split(['-', '_', '@', ';', '=']).collect();
    for (i, w) in words.iter().enumerate() {
        let next = words.get(i + 1).copied().unwrap_or("");
        match *w {
            "ks" | "colstrength" => {
                c.strength = match next {
                    "level1" | "primary" => 1,
                    "level2" | "secondary" => 2,
                    _ => 3,
                }
            }
            "kn" | "colnumericordering" => c.numeric = matches!(next, "true" | "yes" | ""),
            "kf" | "colcasefirst" => c.upper_first = next == "upper",
            _ => {}
        }
    }
    c
}

/// The collation a name stands for, or an error when there is none.
pub fn lookup(name: &str) -> Result<Collation> {
    let key = name.trim_matches('"').to_ascii_lowercase();
    // a schema-qualified name: pg_catalog."C"
    let key = key.rsplit('.').next().unwrap_or(&key).trim_matches('"').to_string();
    if let Some(c) = USER.with(|u| u.borrow().get(&key).cloned()) {
        return Ok(c);
    }
    let binary = |n: &str| Collation { name: n.to_string(), locale: "C".into(), binary: true, deterministic: true, strength: 3, numeric: false, upper_first: false };
    Ok(match key.as_str() {
        "c" | "posix" | "ucs_basic" | "c.utf8" | "c.utf-8" => binary(&key),
        "default" => Collation { name: "default".into(), locale: String::new(), binary: false, deterministic: true, strength: 0, numeric: false, upper_first: false },
        "unicode" => from_options("unicode", "und", true, "icu"),
        "pg_c_utf8" => binary(&key),
        "pg_unicode_fast" => from_options("pg_unicode_fast", "und", true, "icu"),
        k if k.ends_with("-x-icu") => from_options(name, &k[..k.len() - 6], true, "icu"),
        k if is_libc_name(k) => from_options(name, k.split('.').next().unwrap_or(k), true, "libc"),
        _ => return Err(Error::catalog(format!("collation \"{}\" for encoding \"UTF8\" does not exist", name.trim_matches('"')))),
    })
}

fn is_libc_name(k: &str) -> bool {
    // en_US.utf8, de_DE.UTF-8, sv_SE
    let (lang, enc) = match k.split_once('.') {
        Some(p) => p,
        None => return k.len() == 5 && k.as_bytes()[2] == b'_' && k.chars().all(|c| c.is_ascii_alphabetic() || c == '_'),
    };
    matches!(enc, "utf8" | "utf-8") && lang.len() >= 2 && lang.chars().all(|c| c.is_ascii_alphabetic() || c == '_')
}

/// The collations that exist from the start, as `(name, provider)` for `pg_collation`.
pub fn builtins() -> Vec<(&'static str, char)> {
    vec![
        ("default", 'd'),
        ("C", 'c'),
        ("POSIX", 'c'),
        ("ucs_basic", 'c'),
        ("unicode", 'i'),
        ("und-x-icu", 'i'),
        ("en-x-icu", 'i'),
        ("en-US-x-icu", 'i'),
        ("de-x-icu", 'i'),
        ("fr-x-icu", 'i'),
        ("es-x-icu", 'i'),
        ("sv-x-icu", 'i'),
        ("tr-x-icu", 'i'),
        ("ja-x-icu", 'i'),
        ("en_US", 'c'),
        ("en_US.utf8", 'c'),
    ]
}

/// Does an ICU locale name a language that exists? (`nonsense-nowhere` does not.)
pub fn known_locale(locale: &str) -> bool {
    const LANGUAGES: &[&str] = &[
        "und", "en", "de", "fr", "es", "it", "pt", "nl", "sv", "da", "nb", "no", "nn", "fi", "is", "pl", "cs", "sk", "sl", "hr", "sr", "bs", "hu", "ro", "bg", "ru", "uk",
        "be", "el", "tr", "az", "ar", "he", "fa", "hi", "bn", "ta", "th", "vi", "id", "ms", "zh", "ja", "ko", "lt", "lv", "et", "ca", "eu", "gl", "cy", "ga", "mt", "af",
        "sq", "mk", "ka", "hy", "kk", "uz", "mn", "sw", "zu", "fil", "haw", "ee", "fo", "kl", "lb",
    ];
    let lower = locale.to_ascii_lowercase();
    let lang = lower.trim_start_matches('@').split(['-', '_', '@', ';', '=']).next().unwrap_or("");
    lower.starts_with('@') || lang.is_empty() || LANGUAGES.contains(&lang)
}

/// A collation made by `CREATE COLLATION`, as the JSON the catalog keeps.
pub fn definition(locale: &str, deterministic: bool, provider: &str) -> String {
    serde_json::json!({ "locale": locale, "deterministic": deterministic, "provider": provider }).to_string()
}

// ----- sort keys -----------------------------------------------------------------------------------------------

/// Letters that carry an accent, with the plain letter they are written from: the position in the string is the accent.
const ACCENTED: &[(char, &str)] = &[
    ('a', "àáâãäåāăąǎ"),
    ('c', "çćĉċč"),
    ('d', "ďđ"),
    ('e', "èéêëēĕėęě"),
    ('g', "ĝğġģ"),
    ('h', "ĥħ"),
    ('i', "ìíîïĩīĭįǐ"),
    ('j', "ĵ"),
    ('k', "ķ"),
    ('l', "ĺļľŀł"),
    ('n', "ñńņňŉ"),
    ('o', "òóôõöøōŏőǒ"),
    ('r', "ŕŗř"),
    ('s', "śŝşšș"),
    ('t', "ţťŧț"),
    ('u', "ùúûüũūŭůűųǔ"),
    ('w', "ŵ"),
    ('y', "ýÿŷ"),
    ('z', "źżž"),
];

fn decompose(c: char) -> (char, u16) {
    for (base, marked) in ACCENTED {
        if let Some(i) = marked.chars().position(|m| m == c) {
            return (*base, i as u16 + 1);
        }
    }
    (c, 0)
}

/// The primary weight a letter gets in a locale whose alphabet puts it elsewhere (`å` after `z` in Swedish).
fn tailored(lang: &str, c: char) -> Option<u32> {
    let after = |base: char, step: u32| base as u32 * 4 + step;
    match (lang, c) {
        ("sv" | "fi", 'å') => Some(after('z', 1)),
        ("sv" | "fi", 'ä') => Some(after('z', 2)),
        ("sv" | "fi", 'ö') => Some(after('z', 3)),
        ("da" | "nb" | "no", 'æ') => Some(after('z', 1)),
        ("da" | "nb" | "no", 'ø') => Some(after('z', 2)),
        ("da" | "nb" | "no", 'å') => Some(after('z', 3)),
        ("es", 'ñ') => Some(after('n', 2)),
        ("de", 'ß') => Some(after('s', 1)),
        _ => None,
    }
}

impl Collation {
    /// The text reduced to what this collation tells apart, for `LIKE` under a non-deterministic collation: the case
    /// (strength 2 and below) and the accents (strength 1) are dropped. In a pattern the wildcards stay as they are.
    pub fn fold(&self, text: &str, pattern: bool) -> String {
        if self.binary || self.strength >= 3 {
            return text.to_string();
        }
        text.chars()
            .map(|c| {
                if pattern && matches!(c, '%' | '_' | '\\') {
                    return c;
                }
                let lower = c.to_lowercase().next().unwrap_or(c);
                if self.strength == 1 {
                    decompose(lower).0
                } else {
                    lower
                }
            })
            .collect()
    }

    /// The bytes that stand for `text` in this collation; texts the collation puts in the same place get the same bytes.
    pub fn key(&self, text: &str) -> Vec<u8> {
        if self.binary {
            return text.as_bytes().to_vec();
        }
        if self.strength == 0 {
            // the default collation keeps the engine's own order: case-insensitive, then by bytes
            let mut k: Vec<u8> = text.to_lowercase().into_bytes();
            k.push(0);
            k.extend_from_slice(text.as_bytes());
            return k;
        }
        let lang = self.locale.to_ascii_lowercase();
        let lang = lang.split(['-', '_', '@']).next().unwrap_or("").to_string();
        let (mut primary, mut secondary, mut tertiary) = (Vec::new(), Vec::new(), Vec::new());
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let ch = chars[i];
            let upper = ch.is_uppercase();
            let lower = ch.to_lowercase().next().unwrap_or(ch);
            if self.numeric && lower.is_ascii_digit() {
                // a run of digits is one number: shorter (after the leading zeros) is smaller
                let start = i;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                let digits: String = chars[start..i].iter().collect::<String>().trim_start_matches('0').to_string();
                primary.extend_from_slice(&((2u32 << 24) | digits.len() as u32).to_be_bytes());
                for d in digits.chars() {
                    primary.extend_from_slice(&(d as u32).to_be_bytes());
                }
                secondary.extend_from_slice(&[0, 0]);
                tertiary.push(0);
                continue;
            }
            i += 1;
            let (weight, accent) = match tailored(&lang, lower) {
                Some(w) => ((3u32 << 24) | w, 0),
                None => {
                    let (base, accent) = decompose(lower);
                    let w = if base.is_alphabetic() {
                        (3u32 << 24) | (base as u32 * 4)
                    } else if base.is_numeric() {
                        (2u32 << 24) | base.to_digit(10).unwrap_or(base as u32)
                    } else {
                        (1u32 << 24) | base as u32
                    };
                    (w, accent)
                }
            };
            primary.extend_from_slice(&weight.to_be_bytes());
            secondary.extend_from_slice(&accent.to_be_bytes());
            tertiary.push(u8::from(upper) ^ u8::from(self.upper_first));
        }
        let mut key = primary;
        if self.strength >= 2 {
            key.extend_from_slice(&[0, 0, 0, 0]);
            key.extend(secondary);
        }
        if self.strength >= 3 {
            key.extend_from_slice(&[0, 0, 0, 0]);
            key.extend(tertiary);
        }
        if self.deterministic {
            // texts that are equal at every level are still different texts
            key.extend_from_slice(&[0, 0, 0, 0]);
            key.extend_from_slice(text.as_bytes());
        }
        key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(c: &str, words: &[&str]) -> Vec<String> {
        let c = lookup(c).unwrap();
        let mut v: Vec<String> = words.iter().map(|s| s.to_string()).collect();
        v.sort_by_key(|s| c.key(s));
        v
    }

    #[test]
    fn icu_orders_by_letters_then_accents_then_case() {
        assert_eq!(order("en-x-icu", &["bbc", "äbc", "ABC", "abc"]), ["abc", "ABC", "äbc", "bbc"]);
        assert_eq!(order("C", &["bbc", "äbc", "ABC", "abc"]), ["ABC", "abc", "bbc", "äbc"]);
        assert_eq!(order("sv-x-icu", &["zebra", "åre", "apa", "öl"]), ["apa", "zebra", "åre", "öl"]);
    }

    #[test]
    fn strength_and_numbers() {
        let c = from_options("ci", "und-u-ks-level2", false, "icu");
        assert_eq!(c.key("ABC"), c.key("abc"));
        assert_ne!(c.key("äbc"), c.key("abc"));
        let n = from_options("n", "en-u-kn-true", true, "icu");
        assert!(n.key("a2") < n.key("a10"));
        assert!(lookup("nope").is_err());
    }
}
