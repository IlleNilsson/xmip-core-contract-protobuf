//! A `.proto` file read into its messages: proto3, and the proto2 that
//! reads the same — packages, nested messages, enums, `oneof`, `map`,
//! `repeated`, `optional`, `reserved`, `option`, `import`.
//!
//! Imports are recorded and not followed: a type the file does not define
//! is [`Kind::Opaque`], a length-delimited field of any content. Following
//! an import needs a file system and a search path, which is the factory's
//! next layer, not the parser's.

use std::collections::HashMap;

/// What a field's bytes are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `int32`, `int64`, `uint32`, `uint64`, `sint32`, `sint64`, `bool`,
    /// and an enum: wire type 0.
    Varint,
    /// `fixed64`, `sfixed64`, `double`: wire type 1.
    Fixed64,
    /// `fixed32`, `sfixed32`, `float`: wire type 5.
    Fixed32,
    /// `string`: wire type 2, UTF-8.
    Text,
    /// `bytes`: wire type 2, anything.
    Bytes,
    /// An embedded message, by full name: wire type 2, decoded.
    Message(String),
    /// A `map<K, V>`: wire type 2, each entry a message of fields 1 and 2.
    Map(Box<Kind>, Box<Kind>),
    /// A type this file does not define: wire type 2, not looked into.
    Opaque,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub kind: Kind,
    pub repeated: bool,
}

/// One message: its fields by number.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Message {
    pub fields: HashMap<u32, Field>,
}

/// A parsed file: its package, and every message by full name.
#[derive(Clone, Debug, Default)]
pub struct File {
    pub package: String,
    pub messages: HashMap<String, Message>,
    pub imports: Vec<String>,
    enums: Vec<String>,
}

impl File {
    /// Read `text`.
    ///
    /// # Errors
    /// Not a proto file: a statement the language does not have, a field
    /// without a number, a brace that does not close.
    pub fn parse(text: &str) -> Result<Self, String> {
        let tokens = tokenize(text)?;
        let mut parser = Parser {
            tokens,
            at: 0,
            file: Self::default(),
            pending: Vec::new(),
        };
        while !parser.done() {
            parser.statement(&[])?;
        }
        let mut file = parser.file;
        // Type names were kept as written; resolve them now that every
        // message and enum is known.
        let pending = std::mem::take(&mut parser.pending);
        for (message, number, scope, written) in pending {
            let kind = file.resolve(&scope, &written);
            if let Some(field) = file
                .messages
                .get_mut(&message)
                .and_then(|m| m.fields.get_mut(&number))
            {
                field.kind = match std::mem::replace(&mut field.kind, Kind::Opaque) {
                    Kind::Map(key, _) => Kind::Map(key, Box::new(kind)),
                    _ => kind,
                };
            }
        }
        Ok(file)
    }

    /// The message `full_name`, or `None`.
    #[must_use]
    pub fn message(&self, full_name: &str) -> Option<&Message> {
        self.messages
            .get(full_name)
            .or_else(|| self.messages.get(&format!("{}.{full_name}", self.package)))
    }

    /// `written` as seen from inside `scope` (the enclosing message names):
    /// a scalar, a known message or enum in the nearest scope, else opaque.
    fn resolve(&self, scope: &[String], written: &str) -> Kind {
        if let Some(scalar) = scalar(written) {
            return scalar;
        }
        let written = written.trim_start_matches('.');
        let mut candidates = Vec::new();
        for depth in (0..=scope.len()).rev() {
            let mut prefix: Vec<&str> = Vec::new();
            if !self.package.is_empty() {
                prefix.push(&self.package);
            }
            prefix.extend(scope[..depth].iter().map(String::as_str));
            prefix.push(written);
            candidates.push(prefix.join("."));
        }
        candidates.push(written.to_string());
        for candidate in candidates {
            if self.messages.contains_key(&candidate) {
                return Kind::Message(candidate);
            }
            if self.enums.contains(&candidate) {
                return Kind::Varint;
            }
        }
        Kind::Opaque
    }
}

fn scalar(name: &str) -> Option<Kind> {
    Some(match name {
        "int32" | "int64" | "uint32" | "uint64" | "sint32" | "sint64" | "bool" => Kind::Varint,
        "fixed64" | "sfixed64" | "double" => Kind::Fixed64,
        "fixed32" | "sfixed32" | "float" => Kind::Fixed32,
        "string" => Kind::Text,
        "bytes" => Kind::Bytes,
        _ => return None,
    })
}

struct Parser {
    tokens: Vec<String>,
    at: usize,
    file: File,
    /// Fields whose type is a name: message, number, scope, type as written.
    pending: Vec<(String, u32, Vec<String>, String)>,
}

impl Parser {
    fn done(&self) -> bool {
        self.at >= self.tokens.len()
    }

    fn peek(&self) -> &str {
        self.tokens.get(self.at).map_or("", String::as_str)
    }

    fn take(&mut self) -> Result<String, String> {
        let token = self
            .tokens
            .get(self.at)
            .cloned()
            .ok_or_else(|| "the file ends inside a definition".to_string())?;
        self.at += 1;
        Ok(token)
    }

    fn expect(&mut self, token: &str) -> Result<(), String> {
        let found = self.take()?;
        if found == token {
            Ok(())
        } else {
            Err(format!("expected {token:?}, found {found:?}"))
        }
    }

    /// Everything up to and including the next `;`.
    fn skip_statement(&mut self) -> Result<(), String> {
        while self.take()? != ";" {}
        Ok(())
    }

    fn statement(&mut self, scope: &[String]) -> Result<(), String> {
        match self.take()?.as_str() {
            "syntax" | "edition" | "option" | "reserved" | "extensions" => self.skip_statement(),
            "package" => {
                self.file.package = self.take()?;
                self.expect(";")
            }
            "import" => {
                let mut path = self.take()?;
                if path == "public" || path == "weak" {
                    path = self.take()?;
                }
                self.file.imports.push(path.trim_matches('"').to_string());
                self.expect(";")
            }
            "message" => self.message(scope),
            "enum" => {
                let name = self.take()?;
                self.file
                    .enums
                    .push(full_name(&self.file.package, scope, &name));
                self.expect("{")?;
                let mut depth = 1;
                while depth > 0 {
                    match self.take()?.as_str() {
                        "{" => depth += 1,
                        "}" => depth -= 1,
                        _ => {}
                    }
                }
                Ok(())
            }
            ";" => Ok(()),
            other => Err(format!("{other:?} does not begin a statement")),
        }
    }

    fn message(&mut self, scope: &[String]) -> Result<(), String> {
        let name = self.take()?;
        let full = full_name(&self.file.package, scope, &name);
        self.file.messages.entry(full.clone()).or_default();
        let mut inner = scope.to_vec();
        inner.push(name);
        self.expect("{")?;
        loop {
            match self.peek() {
                "}" => {
                    self.at += 1;
                    return Ok(());
                }
                "message" | "enum" | "option" | "reserved" | "extensions" => {
                    self.statement(&inner)?;
                }
                "oneof" => {
                    self.at += 2; // oneof and its name
                    self.expect("{")?;
                    while self.peek() != "}" {
                        if self.peek() == "option" {
                            self.at += 1;
                            self.skip_statement()?;
                        } else {
                            self.field(&full, &inner, false)?;
                        }
                    }
                    self.at += 1;
                }
                "" => return Err(format!("message {full} never closes")),
                _ => self.field(&full, &inner, true)?,
            }
        }
    }

    fn field(&mut self, message: &str, scope: &[String], labels: bool) -> Result<(), String> {
        let mut repeated = false;
        let mut kind_token = self.take()?;
        if labels {
            while matches!(kind_token.as_str(), "repeated" | "optional" | "required") {
                repeated = kind_token == "repeated";
                kind_token = self.take()?;
            }
        }
        let (kind, deferred) = if kind_token == "map" {
            self.expect("<")?;
            let key = self.take()?;
            self.expect(",")?;
            let value = self.take()?;
            self.expect(">")?;
            let key = scalar(&key).unwrap_or(Kind::Varint);
            (
                Kind::Map(Box::new(key), Box::new(Kind::Opaque)),
                Some(value),
            )
        } else if let Some(scalar) = scalar(&kind_token) {
            (scalar, None)
        } else {
            (Kind::Opaque, Some(kind_token))
        };
        let name = self.take()?;
        self.expect("=")?;
        let number: u32 = self
            .take()?
            .parse()
            .map_err(|_| format!("field {name} of {message} has no number"))?;
        if self.peek() == "[" {
            while self.take()? != "]" {}
        }
        self.expect(";")?;
        if let Some(written) = deferred {
            self.pending
                .push((message.to_string(), number, scope.to_vec(), written));
        }
        self.file
            .messages
            .entry(message.to_string())
            .or_default()
            .fields
            .insert(
                number,
                Field {
                    name,
                    kind,
                    repeated,
                },
            );
        Ok(())
    }
}

fn full_name(package: &str, scope: &[String], name: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if !package.is_empty() {
        parts.push(package);
    }
    parts.extend(scope.iter().map(String::as_str));
    parts.push(name);
    parts.join(".")
}

/// Names, numbers, strings and the punctuation that matters, comments gone.
fn tokenize(text: &str) -> Result<Vec<String>, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        if c.is_whitespace() {
            at += 1;
        } else if c == '/' && chars.get(at + 1) == Some(&'/') {
            while at < chars.len() && chars[at] != '\n' {
                at += 1;
            }
        } else if c == '/' && chars.get(at + 1) == Some(&'*') {
            let close = chars[at + 2..]
                .windows(2)
                .position(|w| w == ['*', '/'])
                .ok_or_else(|| "a comment that never closes".to_string())?;
            at += close + 4;
        } else if c == '"' || c == '\'' {
            let start = at;
            at += 1;
            while at < chars.len() && chars[at] != c {
                at += usize::from(chars[at] == '\\') + 1;
            }
            if at >= chars.len() {
                return Err("a string that never closes".to_string());
            }
            at += 1;
            tokens.push(chars[start..at].iter().collect());
        } else if c == '_' || c == '.' || c.is_alphanumeric() || c == '-' {
            let start = at;
            while at < chars.len()
                && (chars[at] == '_'
                    || chars[at] == '.'
                    || chars[at] == '-'
                    || chars[at].is_alphanumeric())
            {
                at += 1;
            }
            tokens.push(chars[start..at].iter().collect());
        } else {
            tokens.push(c.to_string());
            at += 1;
        }
    }
    Ok(tokens)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const ORDER: &str = r#"
        syntax = "proto3";
        package shop.v1;
        import "google/protobuf/timestamp.proto";
        option java_package = "com.shop";

        /* the order */
        message Order {
            int64 id = 1;
            string customer = 2; // who
            repeated Line lines = 3;
            Status status = 4;
            map<string, bool> tags = 5;
            oneof note { string text = 6; bytes blob = 7; }
            google.protobuf.Timestamp placed = 8;
            Line.Price total = 9;
            reserved 10, 11;
            message Line {
                string sku = 1;
                int32 qty = 2 [deprecated = true];
                Price price = 3;
                message Price { double amount = 1; }
            }
        }
        enum Status { NEW = 0; PAID = 1; }
    "#;

    #[test]
    fn a_file_reads_into_its_messages_with_types_resolved() {
        let file = File::parse(ORDER).expect("parse");
        assert_eq!(file.package, "shop.v1");
        assert_eq!(file.imports, ["google/protobuf/timestamp.proto"]);
        let order = file.message("shop.v1.Order").expect("Order");
        assert_eq!(order.fields.len(), 9);
        assert_eq!(order.fields[&1].kind, Kind::Varint);
        assert_eq!(order.fields[&2].kind, Kind::Text);
        assert_eq!(
            order.fields[&3].kind,
            Kind::Message("shop.v1.Order.Line".into())
        );
        assert!(order.fields[&3].repeated);
        assert_eq!(order.fields[&4].kind, Kind::Varint, "an enum is a varint");
        assert_eq!(
            order.fields[&5].kind,
            Kind::Map(Box::new(Kind::Text), Box::new(Kind::Varint))
        );
        assert_eq!(order.fields[&7].kind, Kind::Bytes);
        assert_eq!(order.fields[&8].kind, Kind::Opaque, "an import is opaque");
        assert_eq!(
            order.fields[&9].kind,
            Kind::Message("shop.v1.Order.Line.Price".into())
        );
        let line = file.message("Order.Line").expect("by short name");
        assert_eq!(
            line.fields[&3].kind,
            Kind::Message("shop.v1.Order.Line.Price".into())
        );
        assert_eq!(line.fields[&2].name, "qty");
    }

    #[test]
    fn what_is_not_a_proto_file_is_refused() {
        assert!(File::parse("message A { string x; }").is_err(), "no number");
        assert!(
            File::parse("message A { string x = 1;").is_err(),
            "unclosed"
        );
        assert!(File::parse("banana A {}").is_err(), "statement");
        assert!(File::parse("/* open").is_err(), "comment");
        assert!(File::parse("option x = \"open;").is_err(), "string");
        let empty = File::parse("syntax = \"proto3\";").expect("no messages is still a file");
        assert!(empty.messages.is_empty());
    }
}
