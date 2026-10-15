//! The BPS text of the native protodef types that have no definition in `protocol.json`:
//! UUID, the four NBT flavours and `lpVec3`. Emitted on demand, with their helpers.
//!
//! The NBT encodings follow prismarine-nbt's `nbt.json` (the definition node-minecraft-protocol
//! compiles): `nbt` is `{type, name, value}`, `anonymousNbt` is `{type, value}`, and the
//! optional flavours treat a first byte of `0` as absent. Lists and compounds recurse through
//! `nbt_payload`, which reads `tag_type` from the block that spreads it.

use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pre {
    Uuid,
    Nbt,
    OptionalNbt,
    AnonymousNbt,
    AnonOptionalNbt,
    LpVec3,
    EntityDelta,
}

impl Pre {
    /// The encoding name the protodef native maps to.
    pub fn bps_name(self) -> &'static str {
        match self {
            Pre::Uuid => "uuid",
            Pre::Nbt => "nbt",
            Pre::OptionalNbt => "optional_nbt",
            Pre::AnonymousNbt => "anonymous_nbt",
            Pre::AnonOptionalNbt => "anon_optional_nbt",
            Pre::LpVec3 => "lp_vec3",
            Pre::EntityDelta => "entity_delta",
        }
    }
}

/// Every name the prelude can define, so generated encodings never reuse one.
pub const PRELUDE_NAMES: &[&str] = &[
    "uuid",
    "nbt",
    "optional_nbt",
    "anonymous_nbt",
    "anon_optional_nbt",
    "lp_vec3",
    "entity_delta",
    "nbt_nothing",
    "nbt_string",
    "nbt_byte_array",
    "nbt_int_array",
    "nbt_long_array",
    "nbt_entry",
    "nbt_compound_body",
    "nbt_list_body",
    "nbt_payload",
];

const UUID: &str = "\
// 128-bit UUID, big endian.
encoding uuid = {
    msb: u64be
    lsb: u64be
}
";

const NBT_COMMON: &str = "\
// --- NBT (prismarine-nbt \"big\"): shared bodies ---

encoding nbt_nothing = { }

encoding nbt_string = {
    byte_len: u16be
    value:    bytes(byte_len) as utf8
}

encoding nbt_byte_array = {
    byte_len: i32be
    data:     bytes(byte_len)
}

encoding nbt_int_array = {
    count: i32be
    data:  bytes(count * 4)
}

encoding nbt_long_array = {
    count: i32be
    data:  bytes(count * 8)
}

// A compound entry: tag type, name, payload. TAG_End (0) ends a compound before this is read.
encoding nbt_entry = {
    tag_type: u8
    name:     nbt_string
    ...nbt_payload
}

encoding nbt_compound_body = {
    entries: nbt_entry[] until 0x00
}

// A list: element type, count, then count payloads without type bytes.
encoding nbt_list_body = {
    elem_type: u8
    count:     i32be
    items: { v: match elem_type {
        0  => nbt_nothing
        1  => i8
        2  => i16be
        3  => i32be
        4  => i64be
        5  => f32be
        6  => f64be
        7  => nbt_byte_array
        8  => nbt_string
        9  => nbt_list_body
        10 => nbt_compound_body
        11 => nbt_int_array
        12 => nbt_long_array
    } }[] count = count
}

// The payload of the tag whose type is the enclosing block's `tag_type`.
encoding nbt_payload = {
    value: match tag_type {
        0  => nbt_nothing
        1  => i8
        2  => i16be
        3  => i32be
        4  => i64be
        5  => f32be
        6  => f64be
        7  => nbt_byte_array
        8  => nbt_string
        9  => nbt_list_body
        10 => nbt_compound_body
        11 => nbt_int_array
        12 => nbt_long_array
    }
}
";

const NBT: &str = "\
// protodef `nbt`: root tag type, root name (read even for TAG_End), payload.
encoding nbt = {
    tag_type: u8
    name:     nbt_string
    ...nbt_payload
}
";

const OPTIONAL_NBT: &str = "\
// protodef `optionalNbt`: a first byte of 0 is the whole value; otherwise a named root.
encoding optional_nbt = {
    tag_type: u8
    if tag_type != 0 {
        name: nbt_string
    }
    ...nbt_payload
}
";

const ANONYMOUS_NBT: &str = "\
// protodef `anonymousNbt` (protocol >= 764 wire): root tag type, then the payload, no name.
encoding anonymous_nbt = {
    tag_type: u8
    ...nbt_payload
}
";

const ANON_OPTIONAL_NBT: &str = "\
// protodef `anonOptionalNbt`: a lone 00 is absent, which is `anonymous_nbt` with TAG_End.
encoding anon_optional_nbt = anonymous_nbt
";

const ENTITY_DELTA: &str = "\
// protodef `entityDelta` (26.3 `VecDelta`): `properties` is the on-ground bit plus the step
// count shifted left once. No steps is one linear delta; otherwise each step is a tick offset
// and a delta.
encoding entity_delta = {
    properties: varint
    if (properties >> 1) == 0 {
        delta_x: i16be
        delta_y: i16be
        delta_z: i16be
    }
    if (properties >> 1) > 0 {
        steps: {
            tick_offset: varint
            delta_x: i16be
            delta_y: i16be
            delta_z: i16be
        }[] count = (properties >> 1)
    }
}
";

const LP_VEC3: &str = "\
// protodef `lpVec3`: one zero byte for the zero vector; otherwise 6 packed bytes, plus a
// VarInt scale extension when bit 2 of the first byte is set.
encoding lp_vec3 = {
    lead: u8
    if lead != 0 {
        mid:    u8
        packed: u32be
        if (lead & 4) != 0 {
            scale_extension: varint
        }
    }
}
";

/// The prelude text for the natives in `used`, with the helpers they need.
pub fn text(used: &BTreeSet<Pre>) -> String {
    let mut out = String::new();
    let any_nbt = used.iter().any(|p| {
        matches!(
            p,
            Pre::Nbt | Pre::OptionalNbt | Pre::AnonymousNbt | Pre::AnonOptionalNbt
        )
    });
    let wants = |p: Pre| used.contains(&p);
    if wants(Pre::Uuid) {
        out.push_str(UUID);
        out.push('\n');
    }
    if wants(Pre::LpVec3) {
        out.push_str(LP_VEC3);
        out.push('\n');
    }
    if wants(Pre::EntityDelta) {
        out.push_str(ENTITY_DELTA);
        out.push('\n');
    }
    if any_nbt {
        out.push_str(NBT_COMMON);
        out.push('\n');
    }
    if wants(Pre::Nbt) {
        out.push_str(NBT);
        out.push('\n');
    }
    if wants(Pre::OptionalNbt) {
        out.push_str(OPTIONAL_NBT);
        out.push('\n');
    }
    // `anon_optional_nbt` aliases `anonymous_nbt`, so it needs it defined.
    if wants(Pre::AnonymousNbt) || wants(Pre::AnonOptionalNbt) {
        out.push_str(ANONYMOUS_NBT);
        out.push('\n');
    }
    if wants(Pre::AnonOptionalNbt) {
        out.push_str(ANON_OPTIONAL_NBT);
        out.push('\n');
    }
    out
}
