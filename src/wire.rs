//! The protobuf wire format, checked against a schema. Finding the fields —
//! tags, wire types, lengths, groups, the format's range of field numbers —
//! is the Foundation's walk, `message::protobuf`, which the protobuf shape
//! sections by too; what is the contract's is holding each field it finds to
//! the `.proto` file: the wire type the schema expects, UTF-8 in a string, an
//! embedded message walked as its type. Unknown fields are left alone, as
//! every decoder must; a known field whose wire type is not the schema's is
//! the departure.
//!
//! This module walked the wire itself until 2026-09-23, by different rules
//! from the shape's: it refused every group, a proto2 one or an unknown field
//! that was one, and let field numbers past 2^29 - 1 through (open-problems.md,
//! problem 25, row c).

use crate::proto::{File, Kind, Message};
use message::Stop;
use message::protobuf::{Reader, WireType, fields};
use message::scan::varint;

/// Walk `bytes` as fields with no schema: every tag sound, every value as
/// long as its wire type says.
///
/// # Errors
/// The first departure, with the byte it was found at.
pub fn walk_bare(bytes: &[u8]) -> Result<(), String> {
    fields(bytes, 0..bytes.len()).map(|_| ()).map_err(placed)
}

/// Walk `bytes` as a `message` of `file`, saying where it stops being one.
///
/// # Errors
/// The first departure, with the path to it.
pub fn walk(bytes: &[u8], message: &Message, file: &File, path: &str) -> Result<(), String> {
    let mut reader = Reader::new(bytes, 0..bytes.len());
    let stopped = |stop| format!("{} at {path}", placed(stop));
    while let Some(tag) = reader.tag().map_err(stopped)? {
        let Some(declared) = message.fields.get(&tag.number) else {
            reader.value(tag).map_err(|stop| {
                format!("{} in unknown field {} at {path}", placed(stop), tag.number)
            })?;
            continue;
        };
        let at = format!("{path}.{}", declared.name);
        let expected = expected(&declared.kind);
        let packed_scalar =
            tag.wire == WireType::Len && expected != WireType::Len && declared.repeated;
        // Judged at the tag, before the value is read: a varint read as a
        // length runs past the end, and the departure is the field, not the
        // end of the message.
        if tag.wire != expected && !packed_scalar {
            return Err(format!(
                "wire type {} where {} is {} at {at}",
                tag.wire.number(),
                declared.name,
                expected.number()
            ));
        }
        let value = &bytes[reader
            .value(tag)
            .map_err(|stop| format!("{} at {at}", placed(stop)))?];
        if packed_scalar {
            packed(value, expected).map_err(|m| format!("{m} in packed {at}"))?;
            continue;
        }
        check(value, &declared.kind, file, &at)?;
    }
    Ok(())
}

/// Hold one field's value to what the schema declares it to be.
fn check(value: &[u8], kind: &Kind, file: &File, at: &str) -> Result<(), String> {
    match kind {
        Kind::Varint | Kind::Fixed64 | Kind::Fixed32 | Kind::Bytes | Kind::Opaque => Ok(()),
        Kind::Text => std::str::from_utf8(value)
            .map(|_| ())
            .map_err(|_| format!("a string that is not UTF-8 at {at}")),
        Kind::Message(name) => match file.messages.get(name) {
            Some(inner) => walk(value, inner, file, at),
            None => walk_bare(value).map_err(|m| format!("{m} at {at}")),
        },
        Kind::Map(key, value_kind) => {
            let mut entry = std::collections::HashMap::new();
            for (number, kind) in [(1, key), (2, value_kind)] {
                entry.insert(
                    number,
                    crate::proto::Field {
                        name: if number == 1 { "key" } else { "value" }.to_string(),
                        kind: (**kind).clone(),
                        repeated: false,
                    },
                );
            }
            walk(value, &Message { fields: entry }, file, at)
        }
    }
}

/// The wire type a field of `kind` travels as.
const fn expected(kind: &Kind) -> WireType {
    match kind {
        Kind::Varint => WireType::Varint,
        Kind::Fixed64 => WireType::I64,
        Kind::Fixed32 => WireType::I32,
        _ => WireType::Len,
    }
}

/// A packed repeated scalar: values of `wire` back to back, the last ending
/// exactly where `value` does.
fn packed(value: &[u8], wire: WireType) -> Result<(), String> {
    let width = match wire {
        WireType::I64 => 8,
        WireType::I32 => 4,
        _ => {
            let mut at = 0;
            while at < value.len() {
                at = varint(value, at).map_err(placed)?.1;
            }
            return Ok(());
        }
    };
    if value.len().is_multiple_of(width) {
        Ok(())
    } else {
        Err(format!("{} bytes of {width}-byte values", value.len()))
    }
}

/// A stop in the walk, with the byte it was found at.
fn placed((reason, at): Stop) -> String {
    format!("{reason} (byte {at})")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::tests::ORDER;
    use codec::varint;
    use message::protobuf::{encode_delimited, encode_tag};

    pub fn order(id: u64, customer: &str, qty: u64) -> Vec<u8> {
        let mut line = encode_delimited(1, b"X001");
        line.extend(encode_tag(2, WireType::Varint));
        line.extend(varint::encode(qty));
        let mut price = encode_tag(1, WireType::I64);
        price.extend_from_slice(&10.5f64.to_le_bytes());
        line.extend(encode_delimited(3, &price));
        let mut out = encode_tag(1, WireType::Varint);
        out.extend(varint::encode(id));
        out.extend(encode_delimited(2, customer.as_bytes()));
        out.extend(encode_delimited(3, &line));
        out.extend(encode_tag(4, WireType::Varint));
        out.extend(varint::encode(1));
        let mut entry = encode_delimited(1, b"vip");
        entry.extend(encode_tag(2, WireType::Varint));
        entry.push(1);
        out.extend(encode_delimited(5, &entry));
        out.extend(encode_delimited(8, &[8, 1]));
        out.extend(encode_tag(99, WireType::I32));
        out.extend_from_slice(&[0, 0, 0, 0]);
        out
    }

    #[test]
    fn a_message_walks_by_its_schema_and_departures_are_placed() {
        let file = File::parse(ORDER).expect("schema");
        let message = file.message("shop.v1.Order").expect("Order");
        let bytes = order(4711, "ACME", 2);
        walk(&bytes, message, &file, "order").expect("sound");
        walk_bare(&bytes).expect("bare");

        let mut wrong_wire = bytes.clone();
        wrong_wire[0] = encode_tag(1, WireType::Len)[0];
        let error = walk(&wrong_wire, message, &file, "order").expect_err("wire");
        assert!(error.contains("where id is 0 at order.id"), "{error}");

        let mut bad_utf8 = bytes.clone();
        let at = 1 + varint::encode(4711).len() + 2;
        bad_utf8[at] = 0xff;
        let error = walk(&bad_utf8, message, &file, "order").expect_err("utf-8");
        assert_eq!(error, "a string that is not UTF-8 at order.customer");

        let mut short = bytes.clone();
        short.truncate(bytes.len() - 2);
        assert!(walk(&short, message, &file, "order").is_err());
        assert!(walk_bare(&short).is_err());

        let mut packed = encode_delimited(3, &[]);
        packed.clear();
        packed.extend(encode_tag(3, WireType::Varint));
        packed.push(7);
        let error = walk(&packed, message, &file, "order").expect_err("lines are messages");
        assert!(error.contains("order.lines"), "{error}");
    }

    #[test]
    fn packed_scalars_groups_and_zero_fields_are_judged() {
        let file = File::parse("message P { repeated int32 v = 1; string s = 2; }").expect("p");
        let message = file.message("P").expect("P");
        let mut packed = encode_delimited(1, &[1, 2, 0x80, 0x01]);
        packed.extend(encode_delimited(2, b"ok"));
        walk(&packed, message, &file, "p").expect("packed");
        let cut = encode_delimited(1, &[0x80]);
        assert!(
            walk(&cut, message, &file, "p").is_err(),
            "a packed varint cut off"
        );
        assert!(
            walk_bare(&encode_tag(1, WireType::Group)).is_err(),
            "a group never closed"
        );
        assert!(
            walk_bare(&encode_tag(0, WireType::Varint)).is_err(),
            "field 0"
        );
        assert!(walk_bare(&[0x80; 11]).is_err(), "eleven bytes");
        assert_eq!(varint::encode(300), [0xac, 0x02]);
    }

    #[test]
    fn a_group_is_walked_and_the_field_range_is_the_format_s() {
        // Row c of problem 25: this contract refused every group and let a
        // field number past 2^29 - 1 through, where the shape did neither.
        // Both now read one walk, and a well-formed group — proto2's, or an
        // unknown field that happens to be one — is walked, not refused.
        let mut group = encode_tag(7, WireType::Group);
        group.extend(encode_tag(1, WireType::Varint));
        group.push(1);
        group.extend(varint::encode((7 << 3) | 4)); // the group's end tag
        walk_bare(&group).expect("a closed group is sound wire format");

        let file = File::parse("message P { string s = 2; }").expect("p");
        let message = file.message("P").expect("P");
        let mut with_unknown_group = group.clone();
        with_unknown_group.extend(encode_delimited(2, b"ok"));
        walk(&with_unknown_group, message, &file, "p").expect("an unknown group is skipped");

        let past_the_range = varint::encode(1 << 32);
        let error = walk_bare(&past_the_range).expect_err("field 2^29");
        assert!(
            error.starts_with("a field number outside the format's range"),
            "{error}"
        );
    }
}
