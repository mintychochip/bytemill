# bytemill-wasm

A WebAssembly bridge for the Bytemill front end and interpreter. It exposes a
small C ABI so a browser can check schemas and decode/encode packets without a
server or filesystem.

## Build

```bash
rustup target add wasm32-unknown-unknown
cargo build -p bytemill-wasm --profile wasm-release --target wasm32-unknown-unknown
```

The artifact is `target/wasm32-unknown-unknown/wasm-release/bytemill_wasm.wasm`
(~445 KB). The mintychochip.dev article copies it to
`public/bytemill/bytemill.wasm`; run `scripts/build-bytemill-wasm.sh` in that
repository after changing this crate or any interpreter code it reaches.

## ABI

The module exports its linear memory and five functions:

| export | meaning |
|---|---|
| `bml_alloc(len) -> ptr` | allocate a request buffer of exactly `len` bytes |
| `bml_free(ptr, len)` | free a request buffer |
| `bml_run(ptr, len) -> ptr` | run one UTF-8 JSON request; returns a response buffer |
| `bml_response_len(ptr) -> len` | payload length stored in the response's first four bytes (little-endian) |
| `bml_free_response(ptr)` | free a response buffer |

A response buffer is `u32_le(payload_len) || payload`. The payload is always a
JSON object:

```json
{"ok": true, "data": { ... }}
{"ok": false, "error": {"kind": "ParseError", "offset": 12, "message": "..."}}
```

## Requests

All schema-taking operations use a single self-contained BML source (imports are
rejected because the bridge has no filesystem).

- `{"op":"version"}`
- `{"op":"check","source":"..."}` — packets, encodings, enums, protocols, routes.
- `{"op":"decode","source":"...","packet":"name","hex":"..."}` — one payload,
  strict (trailing bytes are an error). Returns fields, bytes, re-encoded hex,
  and a round-trip flag.
- `{"op":"decode","source":"...","state":"play","direction":"clientbound","hex":"..."}`
  — one whole frame; returns the route id and the decoded packet.
- `{"op":"encode","source":"...","packet":"name","value":{...}}` — returns hex,
  bytes, decoded fields, and a round-trip flag.
- `{"op":"stages","source":"..."}` — returns the syntax, semantic, lowered,
  and typed compiler phases: declarations, resolved symbols and routes, IR
  types/procedures/frames, and typed codecs.
- `{"op":"targets"}` — returns the compatibility matrix: every typed
  printer, its default standard, and the standards it accepts.

The bridge is deliberately read-only with respect to the filesystem. The
emitters are not exposed; they write directories and belong to the CLI.
