//! Parsed TOON value tree and the parse-error type.

use smol_str::SmolStr;

/// Parsed TOON value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    /// Single line scalar (unquoted or stripped of surrounding quotes).
    Scalar(SmolStr),
    /// Ordered list of values.
    List(Vec<Value>),
    /// Ordered map of key/value pairs.
    Map(Vec<(SmolStr, Value)>),
}

impl Value {
    /// Read a scalar value as a string slice. Returns `None` for lists and
    /// maps.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Scalar(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Read a list value. Returns `None` for scalars and maps.
    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(items) => Some(items),
            _ => None,
        }
    }

    /// Read a map value. Returns `None` for scalars and lists.
    pub fn as_map(&self) -> Option<&[(SmolStr, Value)]> {
        match self {
            Value::Map(entries) => Some(entries),
            _ => None,
        }
    }

    /// Look up an entry by key in a map; returns `None` for non-maps or
    /// missing keys.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_map()?
            .iter()
            .find(|(k, _)| k.as_str() == key)
            .map(|(_, v)| v)
    }

    /// Parse the scalar as a `u32` (decimal).
    pub fn as_u32(&self) -> Option<u32> {
        self.as_str()?.parse().ok()
    }

    /// Parse the scalar as a `u8` from a `0x..` hex literal (per the
    /// `body_version: 0x01` form in the manifest schema).
    pub fn as_u8_hex(&self) -> Option<u8> {
        let s = self.as_str()?;
        let body = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
        u8::from_str_radix(body, 16).ok()
    }
}

/// Parse error from a TOON document.
#[derive(Clone, Debug)]
pub struct ParseError {
    /// 1-based line number where the error was detected. `0` means
    /// "no specific line" (e.g., empty input where a value was expected).
    pub line: u32,
    /// Human-readable message.
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.line == 0 {
            f.write_str(&self.message)
        } else {
            write!(f, "line {}: {}", self.line, self.message)
        }
    }
}

impl std::error::Error for ParseError {}
