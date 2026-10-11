"""Types the compiler implements directly.

Everything else — fixed-width integers, varint, boolean — is an encoding in
`schemas/std/` built out of these. Floats stay here because IEEE-754 is not
integer arithmetic over bytes.
"""

from __future__ import annotations

BARE_PRIMITIVES = frozenset({"u8", "bytes", "rest", "f32be", "f64be"})
