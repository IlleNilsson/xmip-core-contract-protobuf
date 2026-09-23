//! The protobuf wire format, walked by schema without keeping what is read:
//! a tag is a field number and a wire type, and the wire type says how long
//! the value is. Unknown fields are skipped, as every decoder must; a known
//! field whose wire type is not the schema's is the departure.
//!
//! The cursor and the base-128 varint are the capability's, shared with Avro
//! (ADR-0044); what is protobuf's is the length-delimited value and the skip
//! by wire type, added to the cursor as [`Wire`].

use crate::proto::{File, Kind, Message};
pub use contract::varint::Reader;
pub use contract::varint::encode as encode_varint;

/// What reading the wire format adds to the varint cursor.
pub trait Wire<'a> {
    /// A length-delimited value.
    ///
    /// # Errors
    /// A length past the end.
    fn delimited(&mut self) -> Result<&'a [u8], String>;

    /// Walk the value of wire type `wire` without a schema.
    ///
    /// # Errors
    /// A wire type that does not exist, a group, or a value past the end.
    fn skip(&mut self, wire: u64) -> Result<(), String>;
}

impl<'a> Wire<'a> for Reader<'a> {
    fn delimited(&mut self) -> Result<&'a [u8], String> {
        let length = self.varint()?;
        let length = usize::try_from(length).map_err(|_| "a length too large".to_string())?;
        self.take(length, "a length-delimited value")
    }

    fn skip(&mut self, wire: u64) -> Result<(), String> {
        match wire {
            0 => self.varint().map(|_| ()),
            1 => self.take(8, "a fixed64").map(|_| ()),
            2 => self.delimited().map(|_| ()),
            5 => self.take(4, "a fixed32").map(|_| ()),
            3 | 4 => Err("a group, which proto3 does not have".to_string()),
            other => Err(format!("wire type {other} does not exist")),
        }
    }
}

/// Walk `bytes` as fields with no schema: every tag sound, every value as
/// long as its wire type says.
///
/// # Errors
/// The first departure.
pub fn walk_bare(bytes: &[u8]) -> Result<(), String> {
    let mut reader = Reader::new(bytes);
    while !reader.is_done() {
        let tag = reader.varint()?;
        if tag >> 3 == 0 {
            return Err("field number 0".to_string());
        }
        reader.skip(tag & 7)?;
    }
    Ok(())
}

/// Walk `bytes` as a `message` of `file`, saying where it stops being one.
///
/// # Errors
/// The first departure, with the path to it.
pub fn walk(bytes: &[u8], message: &Message, file: &File, path: &str) -> Result<(), String> {
    let mut reader = Reader::new(bytes);
    while !reader.is_done() {
        let tag = reader.varint().map_err(|m| format!("{m} at {path}"))?;
        let number =
            u32::try_from(tag >> 3).map_err(|_| format!("a field number too large at {path}"))?;
        let wire = tag & 7;
        if number == 0 {
            return Err(format!("field number 0 at {path}"));
        }
        let Some(field) = message.fields.get(&number) else {
            reader
                .skip(wire)
                .map_err(|m| format!("{m} in unknown field {number} at {path}"))?;
            continue;
        };
        let at = format!("{path}.{}", field.name);
        let expected = match &field.kind {
            Kind::Varint => 0,
            Kind::Fixed64 => 1,
            Kind::Fixed32 => 5,
            _ => 2,
        };
        if wire == 2 && expected != 2 && field.repeated {
            // A packed repeated scalar: the values back to back.
            let packed = reader.delimited().map_err(|m| format!("{m} at {at}"))?;
            let mut inner = Reader::new(packed);
            while !inner.is_done() {
                inner
                    .skip(expected)
                    .map_err(|m| format!("{m} in packed {at}"))?;
            }
            continue;
        }
        if wire != expected {
            return Err(format!(
                "wire type {wire} where {} is {expected} at {at}",
                field.name
            ));
        }
        match &field.kind {
            Kind::Varint | Kind::Fixed64 | Kind::Fixed32 => {
                reader.skip(wire).map_err(|m| format!("{m} at {at}"))?;
            }
            Kind::Bytes | Kind::Opaque => {
                reader.delimited().map_err(|m| format!("{m} at {at}"))?;
            }
            Kind::Text => {
                let value = reader.delimited().map_err(|m| format!("{m} at {at}"))?;
                std::str::from_utf8(value)
                    .map_err(|_| format!("a string that is not UTF-8 at {at}"))?;
            }
            Kind::Message(name) => {
                let value = reader.delimited().map_err(|m| format!("{m} at {at}"))?;
                match file.messages.get(name) {
                    Some(inner) => walk(value, inner, file, &at)?,
                    None => walk_bare(value).map_err(|m| format!("{m} at {at}"))?,
                }
            }
            Kind::Map(key, value) => {
                let entry = reader.delimited().map_err(|m| format!("{m} at {at}"))?;
                let mut fields = std::collections::HashMap::new();
                for (number, kind) in [(1, key), (2, value)] {
                    fields.insert(
                        number,
                        crate::proto::Field {
                            name: if number == 1 { "key" } else { "value" }.to_string(),
                            kind: (**kind).clone(),
                            repeated: false,
                        },
                    );
                }
                walk(entry, &Message { fields }, file, &at)?;
            }
        }
    }
    Ok(())
}

/// A tag for `number` and `wire`.
#[must_use]
pub fn encode_tag(number: u32, wire: u64) -> Vec<u8> {
    encode_varint((u64::from(number) << 3) | wire)
}

/// A length-delimited field `number` holding `bytes`.
#[must_use]
pub fn encode_delimited(number: u32, bytes: &[u8]) -> Vec<u8> {
    let mut out = encode_tag(number, 2);
    out.extend(encode_varint(bytes.len() as u64));
    out.extend_from_slice(bytes);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::tests::ORDER;

    pub fn order(id: u64, customer: &str, qty: u64) -> Vec<u8> {
        let mut line = encode_delimited(1, b"X001");
        line.extend(encode_tag(2, 0));
        line.extend(encode_varint(qty));
        let mut price = encode_tag(1, 1);
        price.extend_from_slice(&10.5f64.to_le_bytes());
        line.extend(encode_delimited(3, &price));
        let mut out = encode_tag(1, 0);
        out.extend(encode_varint(id));
        out.extend(encode_delimited(2, customer.as_bytes()));
        out.extend(encode_delimited(3, &line));
        out.extend(encode_tag(4, 0));
        out.extend(encode_varint(1));
        let mut entry = encode_delimited(1, b"vip");
        entry.extend(encode_tag(2, 0));
        entry.push(1);
        out.extend(encode_delimited(5, &entry));
        out.extend(encode_delimited(8, &[8, 1]));
        out.extend(encode_tag(99, 5));
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
        wrong_wire[0] = encode_tag(1, 2)[0];
        let error = walk(&wrong_wire, message, &file, "order").expect_err("wire");
        assert!(error.contains("where id is 0 at order.id"), "{error}");

        let mut bad_utf8 = bytes.clone();
        let at = 1 + encode_varint(4711).len() + 2;
        bad_utf8[at] = 0xff;
        let error = walk(&bad_utf8, message, &file, "order").expect_err("utf-8");
        assert_eq!(error, "a string that is not UTF-8 at order.customer");

        let mut short = bytes.clone();
        short.truncate(bytes.len() - 2);
        assert!(walk(&short, message, &file, "order").is_err());
        assert!(walk_bare(&short).is_err());

        let mut packed = encode_delimited(3, &[]);
        packed.clear();
        packed.extend(encode_tag(3, 0));
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
        assert!(walk_bare(&encode_tag(1, 3)).is_err(), "a group");
        assert!(walk_bare(&encode_tag(0, 0)).is_err(), "field 0");
        assert!(walk_bare(&[0x80; 11]).is_err(), "eleven bytes");
        assert_eq!(encode_varint(300), [0xac, 0x02]);
    }
}
