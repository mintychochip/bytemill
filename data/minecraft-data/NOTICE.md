# Vendored minecraft-data protocol files

The `pc/<version>/protocol.json` files here are copies, byte for byte, of the ProtoDef protocol
descriptions in [PrismarineJS/minecraft-data](https://github.com/PrismarineJS/minecraft-data).
`sources.json` records, for every file, the repository, commit, upstream path and SHA-256 it was
taken from; `schemas/versions/manifest.json` records which protocol number each one describes.

Upstream states its licence in the README (`## License`, there is no LICENSE file):

> MIT
>
> Some of the data was extracted manually or automatically from wiki.vg and
> minecraft.gamepedia.com. If required by one of the sources the license might change to
> something more appropriate.

Nothing here is edited. Regenerate the schemas from it with `bpsc import-versions`; do not hand-edit
these files, change `schemas/versions/manifest.json` or the importer instead.
