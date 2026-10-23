//! Types the compiler implements directly, porting `bps/primitives.py`.

pub const BARE_PRIMITIVES: &[&str] = &["u8", "bytes", "rest", "f32be", "f64be", "f32le", "f64le"];

pub fn is_bare_primitive(name: &str) -> bool {
    BARE_PRIMITIVES.contains(&name)
}
