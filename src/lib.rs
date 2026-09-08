#![forbid(unsafe_code)]

//! The Protocol Buffers content contract — a technology of
//! `xmip-core-contract`.
//!
//! Two claims, decided 2026-09-07 (ADR-0042): **well-formedness is a given**
//! and **conformance is a given once a contract is named**.
//!
//! Well-formed here is *sound wire format*: every tag a field number and a
//! wire type that exists, every value as long as its wire type says, no
//! byte over. That is all the bytes can say on their own — protobuf carries
//! no schema — and it is what separates a message from a file that is not
//! one.
//!
//! Conformance is the *message type*: a Location that names this contract
//! with `orders.proto#shop.v1.Order` bound has every Stream walked as that
//! message — each known field's wire type the schema's, every string UTF-8,
//! every embedded message walked as its own type, every map entry a key and
//! a value, unknown fields skipped as the format requires. Imports are not
//! followed: a field of an imported type is length-delimited and not looked
//! into. Following them is the factory's next layer.

pub mod proto;
pub mod wire;

use contract::{
    Contract, ContractDescriptor, ContractError, ContractFactory, ContractId, ValidationIssue,
    ValidationResult,
};
pub use proto::File;
use stream::Stream;

/// The protobuf contract, bare or bound to a message type.
pub struct Protobuf {
    descriptor: ContractDescriptor,
    bound: Option<(File, String)>,
}

impl Protobuf {
    /// Sound wire format, of any message.
    #[must_use]
    pub fn new() -> Self {
        Self {
            descriptor: descriptor("protobuf"),
            bound: None,
        }
    }

    /// Every Stream a `message` of `file`.
    ///
    /// # Errors
    /// A message `file` does not define.
    pub fn of(file: File, message: &str) -> Result<Self, ContractError> {
        let full_name = file
            .message(message)
            .map(|_| message.to_string())
            .filter(|_| file.messages.contains_key(message))
            .or_else(|| {
                let qualified = format!("{}.{message}", file.package);
                file.messages.contains_key(&qualified).then_some(qualified)
            })
            .ok_or_else(|| ContractError {
                message: format!("the file does not define message {message}"),
            })?;
        Ok(Self {
            descriptor: descriptor(&format!("protobuf:{full_name}")),
            bound: Some((file, full_name)),
        })
    }

    /// Whether a message type is bound.
    #[must_use]
    pub fn is_bound(&self) -> bool {
        self.bound.is_some()
    }
}

impl Default for Protobuf {
    fn default() -> Self {
        Self::new()
    }
}

fn descriptor(id: &str) -> ContractDescriptor {
    ContractDescriptor {
        id: ContractId(id.to_string()),
        version: "1".to_string(),
        representation: "application/protobuf".to_string(),
    }
}

impl Contract for Protobuf {
    fn descriptor(&self) -> &ContractDescriptor {
        &self.descriptor
    }

    /// Wire format has no magic; the media type is the only mark it carries.
    fn identify(&self, stream: &Stream) -> Result<bool, ContractError> {
        Ok(stream.media_type().is_some_and(|m| {
            let base = m.split(';').next().unwrap_or("").trim();
            base.eq_ignore_ascii_case("application/protobuf")
                || base.eq_ignore_ascii_case("application/x-protobuf")
                || base.eq_ignore_ascii_case("application/vnd.google.protobuf")
        }))
    }

    fn validate(&self, stream: &Stream) -> Result<ValidationResult, ContractError> {
        let outcome = match &self.bound {
            Some((file, name)) => match file.messages.get(name) {
                Some(message) => wire::walk(stream.bytes(), message, file, name),
                None => Err(format!("the schema lost message {name}")),
            },
            None => wire::walk_bare(stream.bytes()),
        };
        Ok(match outcome {
            Ok(()) => ValidationResult {
                valid: true,
                issues: Vec::new(),
            },
            Err(message) => {
                let (message, path) = match message.rsplit_once(" at ") {
                    Some((message, path)) if self.bound.is_some() => {
                        (message.to_string(), Some(path.to_string()))
                    }
                    _ => (message, None),
                };
                ValidationResult {
                    valid: false,
                    issues: vec![ValidationIssue {
                        code: "malformed".to_string(),
                        message,
                        path,
                    }],
                }
            }
        })
    }
}

/// Loads the contract a Location names: an empty reference is the bare
/// contract, anything else `path/to/file.proto#package.Message`.
pub struct ProtobufFactory;

impl ContractFactory for ProtobufFactory {
    fn technology(&self) -> &'static str {
        "protobuf"
    }

    fn load(&self, reference: &str) -> Result<Box<dyn Contract>, ContractError> {
        let reference = reference.trim();
        if reference.is_empty() {
            return Ok(Box::new(Protobuf::new()));
        }
        let Some((path, message)) = reference.split_once('#') else {
            return Err(ContractError {
                message: format!("{reference:?} is not file.proto#package.Message"),
            });
        };
        let text = std::fs::read_to_string(path).map_err(|error| ContractError {
            message: format!("cannot read {path}: {error}"),
        })?;
        let file = File::parse(&text).map_err(|message| ContractError {
            message: format!("{path} is not a proto file: {message}"),
        })?;
        Ok(Box::new(Protobuf::of(file, message)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::tests::ORDER;
    use crate::wire::{encode_delimited, encode_tag, encode_varint};
    use xcore::StreamId;

    fn order() -> Vec<u8> {
        let mut out = encode_tag(1, 0);
        out.extend(encode_varint(4711));
        out.extend(encode_delimited(2, b"ACME"));
        let mut line = encode_delimited(1, b"X001");
        line.extend(encode_tag(2, 0));
        line.push(2);
        out.extend(encode_delimited(3, &line));
        out
    }

    fn stream(bytes: Vec<u8>, media_type: Option<&str>) -> Stream {
        Stream::new(StreamId::new(1), bytes, media_type.map(str::to_string))
    }

    #[test]
    fn sound_wire_format_holds_bare_and_bound() {
        let bare = Protobuf::new();
        assert!(
            bare.identify(&stream(vec![], Some("application/x-protobuf")))
                .expect("identify")
        );
        assert!(!bare.identify(&stream(order(), None)).expect("identify"));
        assert!(
            bare.validate(&stream(order(), None))
                .expect("validate")
                .valid
        );
        assert!(
            bare.validate(&stream(vec![], None))
                .expect("validate")
                .valid
        );
        let file = File::parse(ORDER).expect("schema");
        let bound = Protobuf::of(file, "Order").expect("bound");
        assert!(bound.is_bound());
        assert_eq!(bound.descriptor().id.0, "protobuf:shop.v1.Order");
        assert!(
            bound
                .validate(&stream(order(), None))
                .expect("validate")
                .valid
        );
        assert!(Protobuf::of(File::parse(ORDER).expect("schema"), "Nope").is_err());
    }

    #[test]
    fn a_departure_is_named_with_its_path() {
        let file = File::parse(ORDER).expect("schema");
        let bound = Protobuf::of(file, "shop.v1.Order").expect("bound");
        let mut bad = order();
        bad[0] = encode_tag(2, 0)[0]; // customer as a varint
        let result = bound.validate(&stream(bad, None)).expect("validate");
        assert!(!result.valid);
        assert_eq!(result.issues[0].code, "malformed");
        assert_eq!(
            result.issues[0].path.as_deref(),
            Some("shop.v1.Order.customer")
        );
        let result = Protobuf::new()
            .validate(&stream(vec![0x80], None))
            .expect("validate");
        assert!(!result.valid);
        assert!(result.issues[0].path.is_none());
    }

    #[test]
    fn the_factory_reads_a_file_and_names_a_message() {
        let dir = std::env::temp_dir().join("xmip-protobuf-test");
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("orders.proto");
        std::fs::write(&path, ORDER).expect("write");
        let reference = format!("{}#shop.v1.Order", path.to_str().expect("path"));
        let loaded = ProtobufFactory.load(&reference).expect("load");
        assert_eq!(loaded.descriptor().id.0, "protobuf:shop.v1.Order");
        assert!(
            loaded
                .validate(&stream(order(), None))
                .expect("validate")
                .valid
        );
        assert!(ProtobufFactory.load("orders.proto").is_err(), "no #");
        assert!(ProtobufFactory.load("/no/such.proto#A").is_err());
        assert!(
            !ProtobufFactory
                .load(" ")
                .expect("bare")
                .descriptor()
                .id
                .0
                .contains(':')
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
