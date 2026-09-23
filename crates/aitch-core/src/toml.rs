//! A hand-written parser for the subset of TOML this codebase's own config
//! files actually use, replacing the `toml`/`serde` crates per
//! [`PLAN-ZERO-DEP.md`](../../../../PLAN-ZERO-DEP.md) §2/§4 Phase 7.
//!
//! The subset: `#` comments (to end of line, wherever they appear outside a
//! string), blank lines, top-level `key = value` pairs, one level of
//! `[section]` tables, one level of `[[array of tables]]` (repeated blocks —
//! what `keymaps/nano.toml`/`modern.toml`'s dozens of `[[binding]]` entries
//! are), and scalar values: basic strings `"..."` (with `\"`/`\\`/`\n`/`\t`/
//! `\r` escapes), literal strings `'...'` (no escapes at all — the TOML
//! feature that exists specifically so a Windows path doesn't need every
//! backslash doubled, e.g. `keymap = 'C:\Users\me\mine.toml'`), booleans,
//! integers, floats, and single-line arrays of any of those.
//!
//! **Not supported, and not needed by anything this parses:** dotted keys
//! (`a.b = 1`), inline tables (`{ a = 1 }`), multi-line arrays, quoted keys,
//! nesting deeper than one level (`[[a.b]]`), dates/times, and multi-line
//! strings (`"""..."""`/`'''...'''`). Every one of these would need real
//! code to add and none is exercised by `aitchrc.toml`'s schema
//! ([`crate::config`]) or the shipped keymap files
//! ([`crate::keymap::Keymap::from_toml`]) — a config parser answers to its
//! own config's shape, not to the TOML specification.

use std::fmt;

/// A parsed TOML value. Tables keep insertion order and allow duplicate
/// lookups to just find the first match — real TOML rejects a duplicate key
/// outright, and so does [`parse`], so which policy `get` would use never
/// actually matters in practice; order is kept because "the file's own
/// order" is the only order a hand-rolled map would beat a `Vec` on, and a
/// `Vec` needs no hashing to build.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Table(Vec<(String, Value)>),
    Array(Vec<Value>),
    String(String),
    Bool(bool),
    Integer(i64),
    Float(f64),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Table(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_table(&self) -> Option<&[(String, Value)]> {
        match self {
            Value::Table(entries) => Some(entries),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Value::Integer(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Integer(i) => Some(*i as f64),
            _ => None,
        }
    }

    /// `as_str`, but an error message naming `key` instead of `None` —
    /// shared by `config.rs` and `keymap.rs`'s "read this schema out of a
    /// parsed document" code, which both want the same "wrong type" wording.
    pub fn expect_str(&self, key: &str) -> Result<&str, String> {
        self.as_str()
            .ok_or_else(|| format!("`{key}` should be a string"))
    }

    pub fn expect_bool(&self, key: &str) -> Result<bool, String> {
        self.as_bool()
            .ok_or_else(|| format!("`{key}` should be true or false"))
    }

    pub fn expect_integer(&self, key: &str) -> Result<i64, String> {
        self.as_integer()
            .ok_or_else(|| format!("`{key}` should be an integer"))
    }

    pub fn expect_float(&self, key: &str) -> Result<f64, String> {
        self.as_float()
            .ok_or_else(|| format!("`{key}` should be a number"))
    }

    pub fn expect_table(&self, key: &str) -> Result<&[(String, Value)], String> {
        self.as_table()
            .ok_or_else(|| format!("`{key}` should be a table"))
    }

    pub fn expect_array(&self, key: &str) -> Result<&[Value], String> {
        self.as_array()
            .ok_or_else(|| format!("`{key}` should be an array"))
    }

    pub fn expect_string_array(&self, key: &str) -> Result<Vec<String>, String> {
        self.expect_array(key)?
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("`{key}` should be an array of strings"))
            })
            .collect()
    }

    /// A short name for error messages: `"a string"`, not `"Value::String"`.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Table(_) => "a table",
            Value::Array(_) => "an array",
            Value::String(_) => "a string",
            Value::Bool(_) => "a boolean",
            Value::Integer(_) => "an integer",
            Value::Float(_) => "a float",
        }
    }
}

/// Something went wrong parsing. Carries a human-readable, one-line message
/// (this codebase's config errors are shown on a one-line status bar, not a
/// diagnostic pane — see [`crate::config::ConfigError`]) and the 1-based
/// line it happened on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TomlError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for TomlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for TomlError {}

fn err(line: usize, message: impl Into<String>) -> TomlError {
    TomlError {
        line,
        message: message.into(),
    }
}

/// Parse a whole document into its root table.
pub fn parse(text: &str) -> Result<Value, TomlError> {
    let mut root: Vec<(String, Value)> = Vec::new();
    // The name of the `[section]` or `[[array of tables]]` currently being
    // filled in, or `None` for the root table itself. One level deep is all
    // this subset supports — see the module doc.
    let mut current: Option<String> = None;

    for (index, raw_line) in text.lines().enumerate() {
        let line_no = index + 1;
        let line = strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }

        if let Some(name) = strip_brackets(line, "[[", "]]") {
            let name = check_key(name, line_no)?;
            open_array_table(&mut root, name, line_no)?;
            current = Some(name.to_string());
            continue;
        }
        if let Some(name) = strip_brackets(line, "[", "]") {
            let name = check_key(name, line_no)?;
            open_table(&mut root, name, line_no)?;
            current = Some(name.to_string());
            continue;
        }

        let (key, rest) = line
            .split_once('=')
            .ok_or_else(|| err(line_no, format!("expected `key = value`, found `{line}`")))?;
        let key = check_key(key.trim(), line_no)?;
        let value = parse_value(rest.trim(), line_no)?;
        insert(&mut root, current.as_deref(), key, value, line_no)?;
    }

    Ok(Value::Table(root))
}

/// `[name]`/`[[name]]` header text between the brackets, or `None` if `line`
/// isn't that kind of header at all (an ordinary `key = value` line, most of
/// the time).
fn strip_brackets<'a>(line: &'a str, open: &str, close: &str) -> Option<&'a str> {
    // `[[x]]` must not be read as a `[section]` header whose name happens to
    // start and end with `[`/`]` — checking the doubled form first in the
    // caller (see `parse`'s own order) is what keeps that straight, so this
    // function itself only ever needs to check one bracket width at a time.
    line.strip_prefix(open)?.strip_suffix(close)
}

fn check_key(key: &str, line_no: usize) -> Result<&str, TomlError> {
    if key.is_empty()
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(err(
            line_no,
            format!("`{key}` is not a plain key (quoted/dotted keys are not supported)"),
        ));
    }
    Ok(key)
}

/// Ensure `root[name]` is a `Value::Array`, appending a fresh empty table to
/// it — what a `[[name]]` header does every time it's seen, including the
/// first.
fn open_array_table(
    root: &mut Vec<(String, Value)>,
    name: &str,
    line_no: usize,
) -> Result<(), TomlError> {
    match root.iter_mut().find(|(k, _)| k == name) {
        Some((_, Value::Array(items))) => {
            items.push(Value::Table(Vec::new()));
            Ok(())
        }
        Some((_, other)) => Err(err(
            line_no,
            format!(
                "`{name}` is already {}, not an array of tables",
                other.type_name()
            ),
        )),
        None => {
            root.push((
                name.to_string(),
                Value::Array(vec![Value::Table(Vec::new())]),
            ));
            Ok(())
        }
    }
}

/// Ensure `root[name]` is a `Value::Table`, creating an empty one the first
/// time `[name]` is seen. A second `[name]` reuses the same table (real TOML
/// merges repeated section headers the same way) rather than erroring or
/// overwriting — nothing this parses actually repeats a plain section, but
/// there's no reason to reject it either.
fn open_table(
    root: &mut Vec<(String, Value)>,
    name: &str,
    line_no: usize,
) -> Result<(), TomlError> {
    match root.iter().find(|(k, _)| k == name) {
        Some((_, Value::Table(_))) => Ok(()),
        Some((_, other)) => Err(err(
            line_no,
            format!("`{name}` is already {}, not a table", other.type_name()),
        )),
        None => {
            root.push((name.to_string(), Value::Table(Vec::new())));
            Ok(())
        }
    }
}

/// Insert `key = value` into the root table, or into whichever `[section]`/
/// `[[array of tables]]`'s most recent entry `current` names.
fn insert(
    root: &mut Vec<(String, Value)>,
    current: Option<&str>,
    key: &str,
    value: Value,
    line_no: usize,
) -> Result<(), TomlError> {
    let table = match current {
        None => root,
        Some(name) => match root.iter_mut().find(|(k, _)| k == name) {
            Some((_, Value::Table(t))) => t,
            Some((_, Value::Array(items))) => match items.last_mut() {
                Some(Value::Table(t)) => t,
                _ => unreachable!("open_array_table always leaves a table as the last item"),
            },
            _ => unreachable!("current always names something open_table/open_array_table made"),
        },
    };
    if table.iter().any(|(k, _)| k == key) {
        return Err(err(line_no, format!("`{key}` is set more than once")));
    }
    table.push((key.to_string(), value));
    Ok(())
}

/// Everything before the first `#` that is not inside a `"..."`/`'...'`
/// string — a `#` inside either kind of string is just a character, not a
/// comment starting.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_string: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        match in_string {
            Some(quote) if bytes[i] == quote => in_string = None,
            Some(b'"') if bytes[i] == b'\\' => i += 1, // skip an escaped byte
            Some(_) => {}
            None if bytes[i] == b'"' || bytes[i] == b'\'' => in_string = Some(bytes[i]),
            None if bytes[i] == b'#' => return &line[..i],
            None => {}
        }
        i += 1;
    }
    line
}

fn parse_value(text: &str, line_no: usize) -> Result<Value, TomlError> {
    if let Some(inner) = text.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return unescape(inner, line_no).map(Value::String);
    }
    if let Some(inner) = text.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        return Ok(Value::String(inner.to_string()));
    }
    if text == "true" {
        return Ok(Value::Bool(true));
    }
    if text == "false" {
        return Ok(Value::Bool(false));
    }
    if let Some(inner) = text.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        return parse_array(inner, line_no);
    }
    if text.contains('.') || text.contains('e') || text.contains('E') {
        return text
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| err(line_no, format!("`{text}` is not a valid number")));
    }
    text.parse::<i64>()
        .map(Value::Integer)
        .map_err(|_| err(line_no, format!("`{text}` is not a valid value")))
}

/// A basic string's body: `\"`, `\\`, `\n`, `\t`, `\r` are recognized;
/// anything else after a `\` is an error rather than a silent pass-through,
/// since a config typo here (`\d` meaning "a literal `d`") would be a
/// confusing way to lose a backslash. Use a literal string (`'...'`) for
/// anything that wants its backslashes untouched, e.g. a Windows path.
fn unescape(text: &str, line_no: usize) -> Result<String, TomlError> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other) => {
                return Err(err(line_no, format!("unknown escape `\\{other}`")));
            }
            None => return Err(err(line_no, "trailing `\\` with nothing to escape")),
        }
    }
    Ok(out)
}

/// The inside of a single-line `[...]` array: comma-separated values, a
/// trailing comma allowed, split at top level only (a comma inside a
/// quoted element does not split it).
fn parse_array(inner: &str, line_no: usize) -> Result<Value, TomlError> {
    let inner = inner.trim();
    if inner.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }

    let mut items = Vec::new();
    for piece in split_top_level_commas(inner) {
        let piece = piece.trim();
        if piece.is_empty() {
            continue; // the trailing comma's empty tail
        }
        items.push(parse_value(piece, line_no)?);
    }
    Ok(Value::Array(items))
}

fn split_top_level_commas(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0;
    let mut in_string: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        match in_string {
            Some(quote) if bytes[i] == quote => in_string = None,
            Some(b'"') if bytes[i] == b'\\' => i += 1,
            Some(_) => {}
            None if bytes[i] == b'"' || bytes[i] == b'\'' => in_string = Some(bytes[i]),
            None if bytes[i] == b',' => {
                parts.push(&text[start..i]);
                start = i + 1;
            }
            None => {}
        }
        i += 1;
    }
    parts.push(&text[start..]);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> Value {
        parse(text).unwrap()
    }

    #[test]
    fn plain_scalars() {
        let v = table("a = \"hi\"\nb = true\nc = 42\nd = 3.5\ne = 'raw\\path'\n");
        assert_eq!(v.get("a").unwrap().as_str(), Some("hi"));
        assert_eq!(v.get("b").unwrap().as_bool(), Some(true));
        assert_eq!(v.get("c").unwrap().as_integer(), Some(42));
        assert_eq!(v.get("d").unwrap().as_float(), Some(3.5));
        assert_eq!(v.get("e").unwrap().as_str(), Some("raw\\path"));
    }

    #[test]
    fn a_negative_integer_and_float() {
        let v = table("a = -3\nb = -1.5\n");
        assert_eq!(v.get("a").unwrap().as_integer(), Some(-3));
        assert_eq!(v.get("b").unwrap().as_float(), Some(-1.5));
    }

    #[test]
    fn basic_string_escapes() {
        let v = table(r#"a = "line one\nline two\t\"quoted\"""#);
        assert_eq!(
            v.get("a").unwrap().as_str(),
            Some("line one\nline two\t\"quoted\"")
        );
    }

    #[test]
    fn a_literal_string_keeps_backslashes_untouched() {
        let v = table(r"a = 'C:\Users\me\mine.toml'");
        assert_eq!(v.get("a").unwrap().as_str(), Some(r"C:\Users\me\mine.toml"));
    }

    #[test]
    fn a_string_array() {
        let v = table(r#"a = ["x", "y", "z"]"#);
        let items = v.get("a").unwrap().as_array().unwrap();
        let strs: Vec<&str> = items.iter().map(|i| i.as_str().unwrap()).collect();
        assert_eq!(strs, vec!["x", "y", "z"]);
    }

    #[test]
    fn an_array_with_a_trailing_comma() {
        let v = table("a = [\"x\", \"y\",]\n");
        assert_eq!(v.get("a").unwrap().as_array().unwrap().len(), 2);
    }

    #[test]
    fn an_empty_array() {
        let v = table("a = []\n");
        assert_eq!(v.get("a").unwrap().as_array().unwrap().len(), 0);
    }

    #[test]
    fn a_comma_inside_a_quoted_array_element_does_not_split_it() {
        let v = table(r#"a = ["x, y", "z"]"#);
        let items = v.get("a").unwrap().as_array().unwrap();
        assert_eq!(items[0].as_str(), Some("x, y"));
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let v = table("# a comment\n\na = 1 # trailing too\n\n");
        assert_eq!(v.get("a").unwrap().as_integer(), Some(1));
    }

    #[test]
    fn a_hash_inside_a_string_is_not_a_comment() {
        let v = table(r#"a = "not # a comment""#);
        assert_eq!(v.get("a").unwrap().as_str(), Some("not # a comment"));
    }

    #[test]
    fn a_section_table_groups_its_keys() {
        let v = table("top = 1\n\n[font]\nfamily = \"Mono\"\nsize = 12.0\n");
        assert_eq!(v.get("top").unwrap().as_integer(), Some(1));
        let font = v.get("font").unwrap();
        assert_eq!(font.get("family").unwrap().as_str(), Some("Mono"));
        assert_eq!(font.get("size").unwrap().as_float(), Some(12.0));
    }

    #[test]
    fn array_of_tables_collects_one_entry_per_header() {
        let v = table(
            "[[binding]]\ncommand = \"help\"\n\n[[binding]]\ncommand = \"quit\"\npriority = 5\n",
        );
        let bindings = v.get("binding").unwrap().as_array().unwrap();
        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0].get("command").unwrap().as_str(), Some("help"));
        assert_eq!(bindings[1].get("command").unwrap().as_str(), Some("quit"));
        assert_eq!(bindings[1].get("priority").unwrap().as_integer(), Some(5));
        assert!(bindings[0].get("priority").is_none());
    }

    #[test]
    fn keys_outside_any_array_of_tables_stay_at_the_root() {
        let v = table("profile = \"nano\"\n\n[[binding]]\ncommand = \"help\"\n");
        assert_eq!(v.get("profile").unwrap().as_str(), Some("nano"));
        assert!(v.get("binding").unwrap().as_array().unwrap()[0]
            .get("profile")
            .is_none());
    }

    #[test]
    fn a_duplicate_key_is_an_error() {
        assert!(parse("a = 1\na = 2\n").is_err());
    }

    #[test]
    fn a_line_with_no_equals_sign_is_an_error() {
        assert!(parse("this is not toml at all").is_err());
    }

    #[test]
    fn a_dotted_key_is_rejected_rather_than_misread() {
        assert!(parse("a.b = 1\n").is_err());
    }

    #[test]
    fn an_unterminated_string_is_an_error() {
        assert!(parse("a = \"unterminated\n").is_err());
    }

    #[test]
    fn reusing_a_table_name_as_a_scalar_is_an_error() {
        assert!(parse("a = 1\n[a]\nb = 2\n").is_err());
    }

    #[test]
    fn the_error_names_its_line() {
        let error = parse("a = 1\nb = \n").unwrap_err();
        assert_eq!(error.line, 2);
    }

    #[test]
    fn an_empty_document_is_an_empty_table() {
        assert_eq!(table(""), Value::Table(Vec::new()));
    }
}
