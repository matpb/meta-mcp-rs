//! Extracts the Facebook session cookies from a pasted cURL command or a JSON export.

use std::collections::BTreeMap;

/// Cookies the sidecar accepts; the first three are required.
pub const KEPT: [&str; 7] = ["c_user", "xs", "datr", "sb", "fr", "wd", "presence"];
const REQUIRED: [&str; 3] = ["c_user", "xs", "datr"];

/// Errors never carry cookie values.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CookieError {
    #[error("nothing to parse")]
    Empty,
    #[error("the JSON is not an object of string values or an array of {{name, value}} objects")]
    BadJson,
    #[error("the command has an unterminated quote")]
    UnterminatedQuote,
    #[error("no Cookie header or -b option found in the cURL command")]
    NoCookieHeader,
    #[error("missing cookies: {}", .0.join(", "))]
    Missing(Vec<&'static str>),
}

/// The kept cookies and the browser's user agent when the paste carried one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub cookies: BTreeMap<String, String>,
    pub user_agent: Option<String>,
}

pub fn parse_cookies(input: &str) -> Result<Parsed, CookieError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(CookieError::Empty);
    }
    let mut all: BTreeMap<String, String> = BTreeMap::new();
    let mut user_agent = None;
    if input.starts_with('{') || input.starts_with('[') {
        let value: serde_json::Value =
            serde_json::from_str(input).map_err(|_| CookieError::BadJson)?;
        json_cookies(&value, &mut all)?;
    } else {
        let tokens = tokenize(input)?;
        let mut found = false;
        for header in cookie_strings(&tokens) {
            found = true;
            for pair in header.split(';') {
                if let Some((name, value)) = pair.trim().split_once('=') {
                    all.entry(name.trim().to_owned())
                        .or_insert_with(|| value.trim().to_owned());
                }
            }
        }
        if !found {
            return Err(CookieError::NoCookieHeader);
        }
        user_agent = user_agent_of(&tokens);
    }
    let mut kept = BTreeMap::new();
    let mut missing = Vec::new();
    for name in KEPT {
        match all.remove(name).filter(|v| !v.is_empty()) {
            Some(v) => {
                kept.insert(name.to_owned(), v);
            }
            None if REQUIRED.contains(&name) => missing.push(name),
            None => {}
        }
    }
    if !missing.is_empty() {
        return Err(CookieError::Missing(missing));
    }
    Ok(Parsed {
        cookies: kept,
        user_agent,
    })
}

fn json_cookies(
    value: &serde_json::Value,
    all: &mut BTreeMap<String, String>,
) -> Result<(), CookieError> {
    match value {
        serde_json::Value::Object(map) => {
            for (name, v) in map {
                let s = v.as_str().ok_or(CookieError::BadJson)?;
                all.insert(name.clone(), s.to_owned());
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                let name = item.get("name").and_then(|v| v.as_str());
                let value = item.get("value").and_then(|v| v.as_str());
                let (Some(name), Some(value)) = (name, value) else {
                    return Err(CookieError::BadJson);
                };
                all.insert(name.to_owned(), value.to_owned());
            }
        }
        _ => return Err(CookieError::BadJson),
    }
    Ok(())
}

/// The values of every `Cookie:` header and `-b`/`--cookie` option, in order.
fn cookie_strings(tokens: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for (header, option) in walk(tokens) {
        if let Some(h) = header
            && let Some((name, value)) = h.split_once(':')
            && name.trim().eq_ignore_ascii_case("cookie")
        {
            out.push(value.to_owned());
        }
        // A `-b` argument without `=` is a cookie jar file name, not a cookie.
        if let Some(c) = option
            && c.contains('=')
        {
            out.push(c.to_owned());
        }
    }
    out
}

/// The `User-Agent` header or `-A`/`--user-agent` value, when present.
fn user_agent_of(tokens: &[String]) -> Option<String> {
    for (header, _) in walk(tokens) {
        if let Some(h) = header
            && let Some((name, value)) = h.split_once(':')
            && name.trim().eq_ignore_ascii_case("user-agent")
        {
            let ua = value.trim();
            if !ua.is_empty() {
                return Some(ua.to_owned());
            }
        }
    }
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i].as_str();
        let value = if tok == "-A" || tok == "--user-agent" {
            i += 1;
            tokens.get(i).map(String::as_str)
        } else {
            tok.strip_prefix("--user-agent=")
        };
        if let Some(ua) = value.map(str::trim).filter(|u| !u.is_empty()) {
            return Some(ua.to_owned());
        }
        i += 1;
    }
    None
}

/// For each option position, the header argument and the cookie argument found there.
fn walk(tokens: &[String]) -> Vec<(Option<&str>, Option<&str>)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i].as_str();
        let next = tokens.get(i + 1).map(String::as_str);
        let mut step = 1;
        let header = if tok == "-H" || tok == "--header" {
            step = 2;
            next
        } else {
            tok.strip_prefix("--header=")
                .or_else(|| tok.strip_prefix("-H").filter(|r| !r.is_empty()))
        };
        let option = if tok == "-b" || tok == "--cookie" {
            step = 2;
            next
        } else {
            tok.strip_prefix("--cookie=")
                .or_else(|| tok.strip_prefix("-b").filter(|r| !r.is_empty()))
        };
        out.push((header, option));
        i += step;
    }
    out
}

/// Shell-style word splitting: single quotes, double quotes, `$'..'`, backslash escapes
/// and backslash-newline continuations. Nothing is executed or expanded.
fn tokenize(input: &str) -> Result<Vec<String>, CookieError> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    tokens.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(ch) => cur.push(ch),
                        None => return Err(CookieError::UnterminatedQuote),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.peek().copied() {
                            Some(e @ ('"' | '\\' | '$' | '`')) => {
                                cur.push(e);
                                chars.next();
                            }
                            Some('\n') => {
                                chars.next();
                            }
                            _ => cur.push('\\'),
                        },
                        Some(ch) => cur.push(ch),
                        None => return Err(CookieError::UnterminatedQuote),
                    }
                }
            }
            '$' if chars.peek() == Some(&'\'') => {
                chars.next();
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some('\\') => match chars.next() {
                            Some('n') => cur.push('\n'),
                            Some('r') => cur.push('\r'),
                            Some('t') => cur.push('\t'),
                            Some(e @ ('\\' | '\'' | '"')) => cur.push(e),
                            Some(other) => {
                                cur.push('\\');
                                cur.push(other);
                            }
                            None => return Err(CookieError::UnterminatedQuote),
                        },
                        Some(ch) => cur.push(ch),
                        None => return Err(CookieError::UnterminatedQuote),
                    }
                }
            }
            '\\' => match chars.next() {
                Some('\n') => {}
                Some('\r') if chars.peek() == Some(&'\n') => {
                    chars.next();
                }
                Some(e) => {
                    in_word = true;
                    cur.push(e);
                }
                None => {}
            },
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        tokens.push(cur);
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    const UA: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0";

    fn firefox() -> String {
        format!(
            "curl 'https://www.facebook.com/messages/' --compressed -H 'User-Agent: {UA}' -H 'Accept: */*' -H 'Cookie: sb=sbv; datr=datrv; locale=en_US; c_user=100001; xs=xsv%3A1; fr=frv; wd=1920x1080; presence=C%7B%7D; ps_l=1' -H 'Priority: u=4'"
        )
    }

    #[test]
    fn firefox_copy_as_curl() {
        let p = parse_cookies(&firefox()).expect("parse");
        assert_eq!(p.cookies.len(), 7, "{:?}", p.cookies.keys());
        assert_eq!(p.cookies["c_user"], "100001");
        assert_eq!(p.cookies["xs"], "xsv%3A1");
        assert_eq!(p.cookies["datr"], "datrv");
        assert!(!p.cookies.contains_key("locale") && !p.cookies.contains_key("ps_l"));
        assert_eq!(p.user_agent.as_deref(), Some(UA));
    }

    #[test]
    fn backslash_newline_continuations() {
        let input =
            "curl 'https://x' \\\n  -H 'cookie: c_user=1; xs=2; datr=3' \\\n  -H 'Accept: */*'";
        let p = parse_cookies(input).expect("parse");
        assert_eq!(p.cookies.len(), 3);
        assert_eq!(p.cookies["datr"], "3");
        assert_eq!(p.user_agent, None);
    }

    #[test]
    fn chrome_style_dash_b_and_user_agent_option() {
        let input = r#"curl "https://x" -H "accept: */*" -b "c_user=1; xs=2; datr=3; sb=4; junk=5" -A "Agent/1.0" --compressed"#;
        let p = parse_cookies(input).expect("parse");
        assert_eq!(p.cookies.len(), 4);
        assert_eq!(p.cookies["sb"], "4");
        assert_eq!(p.user_agent.as_deref(), Some("Agent/1.0"));
        let long = "curl 'u' --cookie 'c_user=1; xs=2; datr=3'";
        assert_eq!(parse_cookies(long).expect("long").cookies.len(), 3);
    }

    #[test]
    fn json_object() {
        let input = r#" {"c_user":"1","xs":"2","datr":"3","locale":"x"} "#;
        let p = parse_cookies(input).expect("parse");
        assert_eq!(p.cookies.len(), 3);
        assert_eq!(p.user_agent, None);
        assert_eq!(
            parse_cookies(r#"{"xs": 5}"#).expect_err("non-string"),
            CookieError::BadJson
        );
        assert_eq!(
            parse_cookies("{not json").expect_err("broken"),
            CookieError::BadJson
        );
    }

    #[test]
    fn json_array_of_name_value_objects() {
        let input = r#"[{"name":"c_user","value":"1","domain":".facebook.com"},{"name":"xs","value":"2","httpOnly":true},{"name":"datr","value":"3"},{"name":"fr","value":"9"}]"#;
        let p = parse_cookies(input).expect("parse");
        assert_eq!(p.cookies.len(), 4);
        assert_eq!(p.cookies["fr"], "9");
        assert_eq!(
            parse_cookies(r#"[{"name":"xs"}]"#).expect_err("no value"),
            CookieError::BadJson
        );
        assert_eq!(
            parse_cookies("[1]").expect_err("number"),
            CookieError::BadJson
        );
    }

    #[test]
    fn missing_cookies_are_named_without_values() {
        let input = "curl 'u' -H 'Cookie: sb=topsecret1; fr=topsecret2; wd=topsecret3'";
        let err = parse_cookies(input).expect_err("missing");
        assert_eq!(err, CookieError::Missing(vec!["c_user", "xs", "datr"]));
        let shown = format!("{err} {err:?}");
        assert!(shown.contains("c_user") && shown.contains("xs") && shown.contains("datr"));
        assert!(!shown.contains("topsecret"), "{shown}");
        let one = parse_cookies("curl 'u' -H 'Cookie: c_user=a; xs=b; datr='").expect_err("empty");
        assert_eq!(one, CookieError::Missing(vec!["datr"]));
    }

    #[test]
    fn escaped_single_quote_inside_the_header() {
        let input = r"curl 'u' -H 'Cookie: c_user=1; xs=it'\''s; datr=3'";
        let p = parse_cookies(input).expect("parse");
        assert_eq!(p.cookies["xs"], "it's");
        let ansi = r"curl $'u' -H $'COOKIE: c_user=1; xs=2; datr=3'";
        assert_eq!(parse_cookies(ansi).expect("ansi").cookies.len(), 3);
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(
            parse_cookies("  \n").expect_err("empty"),
            CookieError::Empty
        );
        assert_eq!(
            parse_cookies("curl 'https://x'").expect_err("none"),
            CookieError::NoCookieHeader
        );
        assert_eq!(
            parse_cookies("just some words").expect_err("words"),
            CookieError::NoCookieHeader
        );
        assert_eq!(
            parse_cookies("curl -H 'Cookie: c_user=a").expect_err("quote"),
            CookieError::UnterminatedQuote
        );
    }
}
