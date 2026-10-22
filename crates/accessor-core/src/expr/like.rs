//! `LIKE` matching with Access wildcards (`*` `?` `#` `[set]` `[!set]`) and
//! the ANSI ones (`%` `_`). Case-insensitive. Common shapes get fast paths.

#[derive(Clone, Debug, PartialEq)]
enum Item {
    Any,
    One,
    Digit,
    Lit(char),
    Set { negated: bool, ranges: Vec<(char, char)> },
}

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    /// ASCII-only literal shapes; the needle is stored lowercase.
    Exact(String),
    Prefix(String),
    Suffix(String),
    Contains(String),
    General(Vec<Item>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct LikePattern(Kind);

fn parse_items(pattern: &str) -> Vec<Item> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut items = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' | '%' => {
                if items.last() != Some(&Item::Any) {
                    items.push(Item::Any);
                }
            }
            '?' | '_' => items.push(Item::One),
            '#' => items.push(Item::Digit),
            '[' => {
                if let Some(close) = chars[i + 1..].iter().position(|&c| c == ']').map(|p| p + i + 1) {
                    let mut body = &chars[i + 1..close];
                    let negated = matches!(body.first(), Some('!' | '^'));
                    if negated {
                        body = &body[1..];
                    }
                    let mut ranges = Vec::new();
                    let mut j = 0;
                    while j < body.len() {
                        if j + 2 < body.len() && body[j + 1] == '-' {
                            ranges.push((body[j], body[j + 2]));
                            j += 3;
                        } else {
                            ranges.push((body[j], body[j]));
                            j += 1;
                        }
                    }
                    items.push(Item::Set { negated, ranges });
                    i = close;
                } else {
                    items.push(Item::Lit('['));
                }
            }
            c => items.push(Item::Lit(c)),
        }
        i += 1;
    }
    items
}

fn lit_only(items: &[Item]) -> Option<String> {
    items
        .iter()
        .map(|i| match i {
            Item::Lit(c) if c.is_ascii() => Some(c.to_ascii_lowercase()),
            _ => None,
        })
        .collect()
}

fn eq_ci(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

fn item_matches(item: &Item, c: char) -> bool {
    match item {
        Item::Any => true,
        Item::One => true,
        Item::Digit => c.is_ascii_digit(),
        Item::Lit(l) => eq_ci(*l, c),
        Item::Set { negated, ranges } => {
            let lc = c.to_lowercase().next().unwrap_or(c);
            let hit = ranges.iter().any(|&(lo, hi)| {
                let (lo, hi) = (lo.to_lowercase().next().unwrap_or(lo), hi.to_lowercase().next().unwrap_or(hi));
                lo <= lc && lc <= hi
            });
            hit != *negated
        }
    }
}

fn contains_ci(hay: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || hay.windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle))
}

impl LikePattern {
    pub fn new(pattern: &str) -> Self {
        let items = parse_items(pattern);
        let kind = match items.as_slice() {
            [Item::Any] | [] if pattern.is_empty() => Kind::Exact(String::new()),
            all if all.iter().all(|i| matches!(i, Item::Lit(_))) => {
                lit_only(all).map_or(Kind::General(items.clone()), Kind::Exact)
            }
            [Item::Any, mid @ .., Item::Any] if !mid.is_empty() => {
                lit_only(mid).map_or(Kind::General(items.clone()), Kind::Contains)
            }
            [Item::Any, rest @ ..] if !rest.is_empty() => lit_only(rest).map_or(Kind::General(items.clone()), Kind::Suffix),
            [rest @ .., Item::Any] if !rest.is_empty() => lit_only(rest).map_or(Kind::General(items.clone()), Kind::Prefix),
            _ => Kind::General(items),
        };
        LikePattern(kind)
    }

    pub fn matches(&self, text: &str) -> bool {
        let t = text.as_bytes();
        match &self.0 {
            Kind::Exact(n) => t.eq_ignore_ascii_case(n.as_bytes()),
            Kind::Prefix(n) => t.len() >= n.len() && t[..n.len()].eq_ignore_ascii_case(n.as_bytes()),
            Kind::Suffix(n) => t.len() >= n.len() && t[t.len() - n.len()..].eq_ignore_ascii_case(n.as_bytes()),
            Kind::Contains(n) => contains_ci(t, n.as_bytes()),
            Kind::General(items) => general_match(items, text),
        }
    }
}

fn general_match(items: &[Item], text: &str) -> bool {
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < items.len() {
            if items[p] == Item::Any {
                star = Some((p, t));
                p += 1;
                continue;
            }
            if item_matches(&items[p], text[t]) {
                p += 1;
                t += 1;
                continue;
            }
        }
        match star {
            Some((sp, st)) => {
                p = sp + 1;
                t = st + 1;
                star = Some((sp, st + 1));
            }
            None => return false,
        }
    }
    while p < items.len() && items[p] == Item::Any {
        p += 1;
    }
    p == items.len()
}

#[cfg(test)]
mod tests {
    use super::LikePattern;

    fn m(p: &str, t: &str) -> bool {
        LikePattern::new(p).matches(t)
    }

    #[test]
    fn fast_paths() {
        assert!(m("abc", "ABC"));
        assert!(!m("abc", "abcd"));
        assert!(m("ab*", "ABcd"));
        assert!(m("*cd", "abCD"));
        assert!(m("*bc*", "aBCd"));
        assert!(m("%", "anything"));
        assert!(m("*", ""));
        assert!(m("", ""));
        assert!(!m("", "x"));
    }

    #[test]
    fn general() {
        assert!(m("a?c", "abc"));
        assert!(m("a_c", "aXc"));
        assert!(!m("a?c", "ac"));
        assert!(m("###-##", "123-45"));
        assert!(!m("###-##", "12a-45"));
        assert!(m("[a-c]at", "Bat"));
        assert!(!m("[!a-c]at", "bat"));
        assert!(m("[!a-c]at", "rat"));
        assert!(m("a*b*c", "aXXbYYc"));
        assert!(!m("a*b*c", "aXXbYY"));
        assert!(m("*a*b", "xxaxxb"));
    }
}
