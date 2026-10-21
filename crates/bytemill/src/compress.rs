//! Minecraft-style zlib frame envelopes.
//!
//! Minecraft's compression layer sits outside the schema's logical frame. Once
//! compression is negotiated, every frame on the wire is
//!
//! ```text
//! packet_length: varint   // bytes after this field
//! data_length:   varint   // uncompressed [id][payload] length; 0 = raw
//! data:          bytes    // raw [id][payload], or a zlib blob
//! ```
//!
//! The schema frame (`length, id, payload`) describes the logical frame inside
//! `data`. [`unwrap_minecraft_frame`] turns the wire envelope into that logical
//! frame; [`wrap_minecraft_frame`] goes the other way. The codec is deliberately
//! not part of the schema language: BML describes the stream on one side of the
//! envelope.

use std::io::{Read, Write};

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;

use crate::ast::{BlockItem, FieldType, ResolvedSchema, WireField};
use crate::errors::{BmlResult, decode, encode};

/// `--compression zlib[:threshold]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompressionSpec {
    /// Compress payloads at least this many bytes long; shorter payloads are
    /// written raw with `data_length = 0`. Decode ignores it.
    pub threshold: usize,
}

impl CompressionSpec {
    /// Parse the CLI value. `zlib` means threshold 0 (compress everything);
    /// `zlib:256` compresses payloads of 256 bytes or more.
    pub fn parse(value: &str) -> Result<Self, String> {
        let (codec, threshold) = match value.split_once(':') {
            Some((codec, threshold)) => (codec, Some(threshold)),
            None => (value, None),
        };
        if !codec.eq_ignore_ascii_case("zlib") {
            return Err(format!(
                "unsupported compression codec {codec:?}; only 'zlib' is implemented"
            ));
        }
        let threshold = match threshold {
            Some(text) => text.parse::<usize>().map_err(|_| {
                format!("compression threshold {text:?} is not a non-negative integer")
            })?,
            None => 0,
        };
        Ok(Self { threshold })
    }
}

/// Compression only makes sense for a Minecraft-style schema frame
/// (`length: varint`, `id: varint`, `payload: rest`). Reject anything else
/// before wrapping or unwrapping so a non-Minecraft frame cannot be silently
/// mislabelled.
pub fn ensure_minecraft_frame(
    resolved: &ResolvedSchema,
    state: &str,
    direction: &str,
) -> BmlResult<()> {
    let frame_name = resolved
        .routes
        .iter()
        .find(|(route, _)| route.state == state && route.direction == direction)
        .map(|(route, _)| route.frame.clone().unwrap_or_else(|| "default".into()))
        .ok_or_else(|| decode(format!("no routes in {state}.{direction}")))?;
    let frame = resolved
        .protocols
        .values()
        .filter(|protocol| {
            protocol.routes.iter().any(|route| {
                route.state == state
                    && route.direction == direction
                    && route.frame.clone().unwrap_or_else(|| "default".into()) == frame_name
            })
        })
        .flat_map(|protocol| protocol.frames.iter())
        .find(|frame| frame.name == frame_name)
        .ok_or_else(|| decode(format!("no frame {frame_name}")))?;

    let plain = |field: &WireField, type_name: &str| {
        field.if_expr.is_none()
            && field.repeat.is_none()
            && field.encode_expr.is_none()
            && matches!(
                &field.type_ref,
                FieldType::Ref(ty)
                    if ty.name == type_name && ty.args.is_empty() && ty.transforms.is_empty()
            )
    };
    let mut has_length = false;
    let mut has_id = false;
    let mut has_payload = false;
    for item in &frame.items {
        let BlockItem::Field(field) = item else {
            return Err(decode(
                "compression is only implemented for Minecraft-style frames (varint length/id, payload: rest)",
            ));
        };
        if plain(field, "varint") && field.name == "length" {
            has_length = true;
        } else if plain(field, "varint") && field.name == "id" {
            has_id = true;
        } else if plain(field, "rest") && field.name == "payload" {
            has_payload = true;
        } else {
            return Err(decode(
                "compression is only implemented for Minecraft-style frames (varint length/id, payload: rest)",
            ));
        }
    }
    if has_length && has_id && has_payload {
        Ok(())
    } else {
        Err(decode(
            "compression is only implemented for Minecraft-style frames (varint length/id, payload: rest)",
        ))
    }
}

/// Unwrap a whole Minecraft wire frame: outer length, `data_length`, then
/// either raw bytes or a zlib blob. Returns the logical schema frame
/// (`length: varint`, `id: varint`, `payload: rest`).
pub fn unwrap_minecraft_frame(frame: &[u8]) -> BmlResult<Vec<u8>> {
    let mut pos = 0;
    let packet_len = read_varint(frame, &mut pos)?;
    let end = pos
        .checked_add(packet_len)
        .ok_or_else(|| decode("compressed frame: packet length overflows"))?;
    if end != frame.len() {
        return Err(decode(format!(
            "compressed frame: packet length {packet_len} does not match {} bytes after the length",
            frame.len().saturating_sub(pos)
        )));
    }
    inflate_frame_payload(&frame[pos..end])
}

/// Unwrap the bytes after the outer length and return the raw packet data
/// (`id` plus payload). `game_probe` reads captured frames this way.
pub fn inflate_data(payload: &[u8]) -> BmlResult<Vec<u8>> {
    let mut pos = 0;
    let data_len = read_varint(payload, &mut pos)?;
    let compressed = &payload[pos..];
    let data = if data_len == 0 {
        compressed.to_vec()
    } else {
        let mut decoder = ZlibDecoder::new(compressed);
        let mut out = Vec::new();
        decoder
            .read_to_end(&mut out)
            .map_err(|error| decode(format!("zlib: {error}")))?;
        if out.len() != data_len {
            return Err(decode(format!(
                "compressed frame: data_length says {data_len} bytes, zlib produced {}",
                out.len()
            )));
        }
        out
    };

    if data.is_empty() {
        return Err(decode("compressed frame: empty packet data"));
    }
    Ok(data)
}

/// Unwrap the bytes after the outer length and return a logical schema frame
/// (`length: varint`, `id: varint`, `payload: rest`).
pub fn inflate_frame_payload(payload: &[u8]) -> BmlResult<Vec<u8>> {
    let data = inflate_data(payload)?;
    let mut out = Vec::with_capacity(data.len() + 5);
    write_varint(data.len(), &mut out);
    out.extend_from_slice(&data);
    Ok(out)
}

/// Wrap a logical schema frame (`length: varint`, `id: varint`, `payload: rest`)
/// in a Minecraft compression envelope. A payload shorter than the threshold
/// is written raw with `data_length = 0`.
pub fn wrap_minecraft_frame(frame: &[u8], spec: CompressionSpec) -> BmlResult<Vec<u8>> {
    let mut pos = 0;
    let length = read_varint(frame, &mut pos)?;
    if length != frame.len().saturating_sub(pos) {
        return Err(encode(format!(
            "compressed frame: schema length {length} does not match {} bytes after the length",
            frame.len().saturating_sub(pos)
        )));
    }
    let data = &frame[pos..];
    if data.is_empty() {
        return Err(encode("compressed frame: empty packet data"));
    }

    let mut inner = Vec::new();
    if data.len() < spec.threshold {
        write_varint(0, &mut inner);
        inner.extend_from_slice(data);
    } else {
        write_varint(data.len(), &mut inner);
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(data)
            .map_err(|error| encode(format!("zlib: {error}")))?;
        let compressed = encoder
            .finish()
            .map_err(|error| encode(format!("zlib: {error}")))?;
        inner.extend_from_slice(&compressed);
    }

    let mut out = Vec::with_capacity(inner.len() + 5);
    write_varint(inner.len(), &mut out);
    out.extend_from_slice(&inner);
    Ok(out)
}

fn read_varint(data: &[u8], pos: &mut usize) -> BmlResult<usize> {
    let mut value: u64 = 0;
    let mut shift = 0;
    loop {
        let byte = *data
            .get(*pos)
            .ok_or_else(|| decode("compressed frame: truncated varint"))?;
        *pos += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift >= 64 {
            return Err(decode("compressed frame: varint overflow"));
        }
    }
    usize::try_from(value).map_err(|_| decode("compressed frame: varint is too large"))
}

fn write_varint(mut value: usize, out: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handshake_frame() -> Vec<u8> {
        // README's handshake: length 0x10, id 0, payload 15 bytes.
        vec![
            0x10, 0x00, 0x89, 0x06, 0x09, 0x6c, 0x6f, 0x63, 0x61, 0x6c, 0x68, 0x6f, 0x73, 0x74,
            0x63, 0xdd, 0x01,
        ]
    }

    #[test]
    fn round_trips_a_compressed_frame() {
        let logical = handshake_frame();
        let wrapped = wrap_minecraft_frame(&logical, CompressionSpec { threshold: 0 }).unwrap();
        assert_eq!(unwrap_minecraft_frame(&wrapped).unwrap(), logical);
        // packet_length, data_length, then zlib bytes.
        assert_ne!(wrapped[1], 0);
        assert!(wrapped.len() < logical.len() + 10);
    }

    #[test]
    fn below_threshold_is_written_raw() {
        let logical = handshake_frame();
        let wrapped = wrap_minecraft_frame(&logical, CompressionSpec { threshold: 1024 }).unwrap();
        assert_eq!(unwrap_minecraft_frame(&wrapped).unwrap(), logical);
        assert_eq!(wrapped[1], 0, "data_length 0 means raw");
    }

    #[test]
    fn rejects_a_bad_zlib_blob() {
        // packet_length 2, data_length 8, one garbage byte.
        let bad = [0x02, 0x08, 0x00];
        let error = unwrap_minecraft_frame(&bad).unwrap_err();
        assert_eq!(error.kind, crate::errors::ErrorKind::Decode);
    }

    #[test]
    fn parses_the_cli_spec() {
        assert_eq!(
            CompressionSpec::parse("zlib").unwrap(),
            CompressionSpec { threshold: 0 }
        );
        assert_eq!(
            CompressionSpec::parse("zlib:256").unwrap(),
            CompressionSpec { threshold: 256 }
        );
        assert!(CompressionSpec::parse("zstd").is_err());
        assert!(CompressionSpec::parse("zlib:-1").is_err());
    }
}
