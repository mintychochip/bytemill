# targets/rstom

The inputs of `bml emit rstom-tree`: every packet payload module of rstom's
`crates/netherstar/src/network/packet/` is generated from them. Both are authored source and
tracked by git; nothing here can be recreated from the rstom tree, which holds only the output.

## shapes/

The shape ledger: one `shapes/<group>/<package>/<stem>.json` per BML packet unit (236 files). A shape
records the public API of the hand type and binds its fields to the BML wire fields; see
[bound shapes](../../docs/superpowers/specs/2026-10-30-bml-bound-shapes-generate-every-packet.md).

```sh
bml shapes wires schemas/minecraft_26_3_wiki.bml --rstom ../rstom --class SetSlotPacket
bml shapes render schemas/minecraft_26_3_wiki.bml --rstom ../rstom --class SetSlotPacket
bml shapes fmt                      # rewrite the ledger in its canonical form
bml emit rstom-tree schemas/minecraft_26_3_wiki.bml --rstom ../rstom [--check]
```

## bindings.json

The conversions that hold for a hand type in every packet (`PlayerHand` as its ordinal, `Pos` over five
wire fields, `Component` through its buffer functions, the `Copy` hand types). A shape's own keys win.
