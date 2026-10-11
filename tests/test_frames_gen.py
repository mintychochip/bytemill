"""Generated Rust frame walkers against the interpreter.

A walker has to find where an unprefixed payload ends, which is exactly what the
interpreter does when it decodes one. So the interpreter is the oracle: random
valid payloads (drawn by decoding from a random byte stream) are measured by the
compiled Rust walker, and the two lengths must agree, with trailing bytes after
the payload that the walker must leave alone.
"""

import random
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock
from pathlib import Path

from bps import emit_rstom as em
from bps import frames_gen
from bps.analyze import analyze
from bps.ast import TypeRef
from bps.errors import BpsError, DecodeError
from bps.interp import engine
from bps.interp.cursor import Cursor
from bps.interp.engine import _decode_type_ref
from bps.interp.evalexpr import eval_expr
from bps.parser import parse

from tests.test_varint import load_minecraft

ROOT = Path(__file__).resolve().parents[1]
SAMPLES = 300
TRAILING = bytes([0xAA, 0xBB, 0xCC])
# A recursive encoding can draw a payload of megabytes; keep the samples small.
MAX_PAYLOAD = 160


class _RandomCursor(Cursor):
    """A cursor over an endless random stream: a decode that succeeds consumed a valid payload."""

    def __init__(self, rng: random.Random) -> None:
        super().__init__(b"")
        self.data = bytearray()
        self.rng = rng

    def _byte(self) -> int:
        roll = self.rng.random()
        if roll < 0.5:
            return self.rng.randrange(0, 4)
        if roll < 0.8:
            return self.rng.randrange(4, 17)
        return self.rng.randrange(0x20, 0x7F)

    def remaining(self) -> int:
        # The interpreter skips a true-guarded group when no bytes remain; this stream never ends.
        return 1 << 30

    def _ensure(self, n: int) -> None:
        if self.pos + n > MAX_PAYLOAD:
            raise DecodeError("payload too large for the check")
        while len(self.data) < self.pos + n:
            self.data.append(self._byte())

    def read(self, n: int) -> bytes:
        self._ensure(n)
        return bytes(super().read(n))

    def peek_u8(self) -> int:
        self._ensure(1)
        return super().peek_u8()

    def consumed(self) -> bytes:
        return bytes(self.data[: self.pos])


def _valid_payloads(resolved, name: str, count: int) -> list[bytes]:
    original = engine._decode_repeat

    def bounded(wf, cur, res, env):
        # A count past the payload cap cannot be a valid payload, and a list of
        # zero-width elements would loop that many times without reading a byte.
        if wf.repeat is not None and wf.repeat.mode == "count" and wf.repeat.expr is not None:
            if int(eval_expr(wf.repeat.expr, env)) > MAX_PAYLOAD:
                raise DecodeError("count too large for the check")
        return original(wf, cur, res, env)

    rng = random.Random(name)
    out: list[bytes] = []
    attempts = 0
    with mock.patch.object(engine, "_decode_repeat", bounded):
        while len(out) < count and attempts < count * 60:
            attempts += 1
            cur = _RandomCursor(rng)
            try:
                _decode_type_ref(TypeRef(name), cur, resolved, {})
            except (BpsError, RecursionError, UnicodeDecodeError):
                continue
            out.append(cur.consumed())
    return out


def _hex(data: bytes) -> str:
    return data.hex()


def _differences(resolved, names: dict[str, str], rust_frames: str) -> list[str]:
    """Walker length != interpreter length, over random valid payloads of each name."""
    cases: list[tuple[str, bytes]] = []
    for name in names:
        payloads = _valid_payloads(resolved, name, SAMPLES)
        if len(payloads) < SAMPLES // 4:
            return [f"{name}: only {len(payloads)} random payloads decoded, the check would be empty"]
        cases += [(name, p) for p in payloads]
    arms = "\n".join(f'        "{n}" => frames::{fn}(bytes),' for n, fn in names.items())
    table = "\n".join(
        f'        ("{n}", "{_hex(p + TRAILING)}", {len(p)}),' for n, p in cases
    )
    main = f"""mod frames;

fn walk(name: &str, bytes: &[u8]) -> usize {{
    match name {{
{arms}
        _ => panic!("no walker for {{name}}"),
    }}
}}

fn unhex(s: &str) -> Vec<u8> {{
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}}

fn main() {{
    let cases: &[(&str, &str, usize)] = &[
{table}
    ];
    let mut bad = 0;
    for (name, hex, want) in cases {{
        let bytes = unhex(hex);
        let got = walk(name, &bytes);
        if got != *want {{
            println!("{{name}}: walker {{got}} != interpreter {{want}} for {{hex}}");
            bad += 1;
            if bad > 20 {{
                break;
            }}
        }}
    }}
    if bad > 0 {{
        std::process::exit(1);
    }}
}}
"""
    with tempfile.TemporaryDirectory() as td:
        d = Path(td)
        (d / "frames.rs").write_text(rust_frames, encoding="utf-8")
        (d / "main.rs").write_text(main, encoding="utf-8")
        built = subprocess.run(
            ["rustc", "--edition", "2021", "-A", "warnings", "-O", "main.rs", "-o", "runner"],
            cwd=d,
            capture_output=True,
            text=True,
        )
        if built.returncode != 0:
            return ["rustc failed:\n" + built.stderr[-3000:]]
        ran = subprocess.run([str(d / "runner")], capture_output=True, text=True)
    return [line for line in ran.stdout.splitlines() if line]


@unittest.skipUnless(shutil.which("rustc"), "rustc not installed")
class GeneratedWalkerTest(unittest.TestCase):
    def test_walkers_follow_guards_counts_matches_lengths_and_recursion(self) -> None:
        std = "".join(
            (ROOT / "schemas" / "std" / f"{name}.bps").read_text(encoding="utf-8")
            for name in ("integers", "varint")
        )
        resolved = analyze(
            parse(
                std
                + """
                encoding leaf = {
                    n: u8
                    tail: bytes(n)
                }
                encoding tree = {
                    kind: varint
                    body: match kind {
                        0 => leaf
                        1 => {
                            count: u8
                            kids: tree[] count = count
                        }
                        2 => {
                            flags: u8
                            extra: u16be if flags & 4 != 0
                            more: i32be if (flags >> 1) & 1 == 1
                        }
                        3 => { a: varint, b: boolean, c: varlong }
                        _ => {}
                    }
                }
                packet probe { t: tree }
                """
            )
        )
        walkers = frames_gen.generate(resolved, ["tree"], {})
        self.assertEqual(walkers.unsupported, {})
        self.assertEqual(_differences(resolved, walkers.names, walkers.source), [])

    def test_every_walker_the_protocol_needs_matches_the_interpreter(self) -> None:
        resolved = load_minecraft()
        plan = em._frames(resolved)
        self.assertEqual(plan.walkers.unsupported, {})
        # id_set is reached through its `ingredient` alias; test each encoding once.
        names = {n: fn for n, fn in plan.walkers.names.items() if n != "ingredient"}
        self.assertTrue({"slot_display", "recipe_display", "particle_data", "id_set"} <= set(names))
        rust = em._FRAMES_RS + plan.walkers.source
        self.assertEqual(_differences(resolved, names, rust), [])

    def test_an_until_loop_is_reported_not_guessed(self) -> None:
        std = "".join(
            (ROOT / "schemas" / "std" / f"{name}.bps").read_text(encoding="utf-8")
            for name in ("integers", "varint")
        )
        resolved = analyze(
            parse(std + "encoding run = { items: u8[] until 0 }\npacket probe { r: run }")
        )
        walkers = frames_gen.generate(resolved, ["run"], {})
        self.assertIn("run", walkers.unsupported)
        self.assertEqual(walkers.names, {})


if __name__ == "__main__":
    unittest.main()
