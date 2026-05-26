//! Planner: tiny JSON-array parser for the agent action pipeline.
//!
//! Architecture:
//!   User → LLM → Planner → Executor → OS
//!
//! The LLM is prompted to emit ONLY a JSON array of action objects like
//!   [{"action":"beep","freq":880},
//!    {"action":"say","text":"hello"},
//!    {"action":"http_get","host":"example.com","path":"/"}]
//!
//! parse_plan() walks that text and produces a `Vec<Action>` the executor
//! consumes.  Tolerant: ignores whitespace, accepts both quoted and
//! unquoted simple values, surfaces a precise error for unrecoverable
//! parse failures so the validator-AI can fix them.
//!
//! NOT a general JSON parser — covers the array-of-flat-objects subset.

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug)]
pub struct Action {
    pub verb:   String,
    pub args:   Vec<(String, String)>,
}

impl Action {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.args.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

#[derive(Debug)]
pub struct ParseError {
    pub byte_offset: usize,
    pub message:     &'static str,
}

pub fn parse_plan(text: &str) -> Result<Vec<Action>, ParseError> {
    let bytes = text.as_bytes();
    let mut p = 0;
    skip_ws(bytes, &mut p);

    // Tolerate leading "```json" code fences the LLM sometimes adds.
    if bytes.get(p..p+3) == Some(b"```") {
        // Skip the rest of the fence line.
        while p < bytes.len() && bytes[p] != b'\n' { p += 1; }
        if p < bytes.len() { p += 1; }
        skip_ws(bytes, &mut p);
    }

    expect(bytes, &mut p, b'[')?;
    let mut out = Vec::new();

    loop {
        skip_ws(bytes, &mut p);
        if let Some(&b) = bytes.get(p) {
            if b == b']' { p += 1; break; }
        } else {
            return Err(err(p, "unexpected EOF before ]"));
        }

        let act = parse_object(bytes, &mut p)?;
        out.push(act);

        skip_ws(bytes, &mut p);
        match bytes.get(p) {
            Some(&b',') => { p += 1; }
            Some(&b']') => { p += 1; break; }
            _ => return Err(err(p, "expected , or ] after action")),
        }
    }
    Ok(out)
}

fn parse_object(bytes: &[u8], p: &mut usize) -> Result<Action, ParseError> {
    skip_ws(bytes, p);
    expect(bytes, p, b'{')?;
    let mut args = Vec::new();
    let mut verb: Option<String> = None;

    loop {
        skip_ws(bytes, p);
        if bytes.get(*p) == Some(&b'}') { *p += 1; break; }

        let key = parse_str(bytes, p)?;
        skip_ws(bytes, p);
        expect(bytes, p, b':')?;
        skip_ws(bytes, p);
        let val = parse_value(bytes, p)?;

        if key == "action" {
            verb = Some(val);
        } else {
            args.push((key, val));
        }

        skip_ws(bytes, p);
        match bytes.get(*p) {
            Some(&b',') => { *p += 1; }
            Some(&b'}') => { *p += 1; break; }
            _ => return Err(err(*p, "expected , or } inside action object")),
        }
    }

    let verb = verb.ok_or(err(*p, "object missing \"action\" key"))?;
    Ok(Action { verb, args })
}

fn parse_str(bytes: &[u8], p: &mut usize) -> Result<String, ParseError> {
    skip_ws(bytes, p);
    let opener = *bytes.get(*p).ok_or(err(*p, "EOF in key"))?;
    if opener == b'"' || opener == b'\'' {
        *p += 1;
        let start = *p;
        while *p < bytes.len() && bytes[*p] != opener {
            if bytes[*p] == b'\\' && *p + 1 < bytes.len() {
                *p += 2;
            } else {
                *p += 1;
            }
        }
        if *p >= bytes.len() {
            return Err(err(start, "unterminated string"));
        }
        let s = unescape(&bytes[start..*p]);
        *p += 1;
        Ok(s)
    } else {
        // Unquoted identifier (tolerant for keys like action, app, url …)
        let start = *p;
        while *p < bytes.len() && is_ident(bytes[*p]) { *p += 1; }
        if start == *p {
            return Err(err(*p, "expected string/identifier"));
        }
        Ok(String::from(core::str::from_utf8(&bytes[start..*p]).unwrap_or("?")))
    }
}

fn parse_value(bytes: &[u8], p: &mut usize) -> Result<String, ParseError> {
    skip_ws(bytes, p);
    let lead = *bytes.get(*p).ok_or(err(*p, "EOF in value"))?;
    if lead == b'"' || lead == b'\'' {
        parse_str(bytes, p)
    } else {
        // Bare token (number, bool, identifier).
        let start = *p;
        while *p < bytes.len() && is_value_char(bytes[*p]) { *p += 1; }
        if start == *p {
            return Err(err(*p, "expected value"));
        }
        Ok(String::from(core::str::from_utf8(&bytes[start..*p]).unwrap_or("?")))
    }
}

fn skip_ws(bytes: &[u8], p: &mut usize) {
    while let Some(&b) = bytes.get(*p) {
        if b == b' ' || b == b'\n' || b == b'\r' || b == b'\t' { *p += 1; } else { break; }
    }
}

fn expect(bytes: &[u8], p: &mut usize, want: u8) -> Result<(), ParseError> {
    skip_ws(bytes, p);
    match bytes.get(*p) {
        Some(&b) if b == want => { *p += 1; Ok(()) }
        _ => Err(err(*p, "expected punctuation")),
    }
}

fn err(byte_offset: usize, message: &'static str) -> ParseError {
    ParseError { byte_offset, message }
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-'
}

fn is_value_char(b: u8) -> bool {
    !matches!(b, b',' | b'}' | b']' | b' ' | b'\n' | b'\r' | b'\t')
}

fn unescape(src: &[u8]) -> String {
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        if src[i] == b'\\' && i + 1 < src.len() {
            let c = match src[i + 1] {
                b'n' => '\n',
                b't' => '\t',
                b'r' => '\r',
                b'"' => '"',
                b'\'' => '\'',
                b'\\' => '\\',
                _ => src[i + 1] as char,
            };
            out.push(c);
            i += 2;
        } else if src[i] < 128 {
            out.push(src[i] as char);
            i += 1;
        } else {
            // Skip non-ASCII for simplicity.
            i += 1;
        }
    }
    out
}
