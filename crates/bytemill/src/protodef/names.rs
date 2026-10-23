//! Identifier policy for generated schemas: `snake_case`, no BML keywords, unique per scope.

/// Words the parser reads as syntax where a field or encoding name could stand. `count`, `id`,
/// `match` and `switch` are legal names (a statement keyword needs a following `:` or `=`), and
/// the hand-written schemas use them.
const RESERVED: &[&str] = &[
    "encoding",
    "enum",
    "packet",
    "protocol",
    "frame",
    "route",
    "require",
    "until",
    "until_end",
    "if",
    "as",
    "signed",
    "true",
    "false",
    "rest",
    "import",
    "u8",
    "bytes",
    "f32be",
    "f64be",
    "f32le",
    "f64le",
    "_",
];

/// `entityId` -> `entity_id`, `playerUUID` -> `player_uuid`, `HTTPServer` -> `http_server`.
/// Anything that is not a letter or digit becomes `_`; a leading digit gets an `f` prefix.
pub fn snake(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            out.push('_');
            continue;
        }
        if c.is_uppercase() && i > 0 {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if prev.is_lowercase() || prev.is_ascii_digit() || (prev.is_uppercase() && next_lower) {
                out.push('_');
            }
        }
        out.extend(c.to_lowercase());
    }
    let mut collapsed = String::with_capacity(out.len());
    for c in out.chars() {
        if c == '_' && collapsed.ends_with('_') {
            continue;
        }
        collapsed.push(c);
    }
    let trimmed = collapsed.trim_matches('_');
    let base = if trimmed.is_empty() { "x" } else { trimmed };
    if base.starts_with(|c: char| c.is_ascii_digit()) {
        format!("f{base}")
    } else {
        base.to_string()
    }
}

/// True when `name` cannot be used as a field or encoding name.
pub fn is_reserved(name: &str) -> bool {
    RESERVED.contains(&name)
}

/// The first of `base`, `base_2`, `base_3`, ... that is neither reserved nor `taken`.
pub fn unique(base: &str, taken: impl Fn(&str) -> bool) -> String {
    let first = if is_reserved(base) {
        format!("{base}_")
    } else {
        base.to_string()
    };
    if !taken(&first) {
        return first;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{first}_{n}");
        if !taken(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_splits_camel_case_and_acronyms() {
        assert_eq!(snake("entityId"), "entity_id");
        assert_eq!(snake("playerUUID"), "player_uuid");
        assert_eq!(snake("HTTPServer"), "http_server");
        assert_eq!(snake("SlotComponentType"), "slot_component_type");
        assert_eq!(snake("vec3i16"), "vec3i16");
        assert_eq!(snake("UUID"), "uuid");
        assert_eq!(snake("x"), "x");
        assert_eq!(snake("minecraft:foo"), "minecraft_foo");
        assert_eq!(snake("3d"), "f3d");
        assert_eq!(snake("___"), "x");
    }

    #[test]
    fn unique_avoids_keywords_and_taken_names() {
        assert_eq!(unique("if", |_| false), "if_");
        assert_eq!(unique("a", |n| n == "a"), "a_2");
        assert_eq!(unique("a", |n| n == "a" || n == "a_2"), "a_3");
        assert_eq!(unique("count", |_| false), "count");
    }
}
