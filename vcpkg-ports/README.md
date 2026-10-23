# vcpkg overlay ports

`aom`, `libvpx`, `libyuv` and `opus` are copied unchanged from RustDesk's
`res/vcpkg/` at commit `c7f212df2c960fa74bd83d3849a5a7ce7809a047`
(https://github.com/rustdesk/rustdesk, AGPL-3.0, © Purslane Ltd. and
contributors). They build the static codec libraries the way RustDesk's
`scrap` and `magnum-opus` crates link them (for example `vpx.lib` without a
CRT suffix).

`vcpkg.json` in the repository root pins the same vcpkg baseline as RustDesk's
own build. When moving to a newer RustDesk commit, copy these ports again and
update the baseline to match RustDesk's `vcpkg.json`.
