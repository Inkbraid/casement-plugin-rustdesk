# RustDesk support for Casement

`casement-rustdesk.exe` is a Casement protocol
plugin: it lets Casement open `rustdesk://` addresses in a tab, showing RustDesk
peers that are reachable through **your own, self-hosted RustDesk server**
(`hbbs` ID/rendezvous server and `hbbr` relay, the open-source
[rustdesk-server](https://github.com/rustdesk/rustdesk-server)).

This is an independent project. It is not made or endorsed by the RustDesk
project; "RustDesk" names the protocol and software it works with.

It never uses the public rustdesk.com servers, has no account login and no
address book. Casement keeps the connection list and saved passwords; the
plugin only connects, shows the picture and passes input and clipboard text.

## Status

| | |
|---|---|
| Plugin protocol v1 (framing, start, states, surfaces, frame flow control, input, cursor, clipboard) | done |
| `rustdesk://test` test pattern for testing Casement's side | done |
| RustDesk peers via a self-hosted hbbs/hbbr | in progress |
| Hole punching / direct LAN, audio, `resize`, multiple displays | later |

See [CHANGELOG.md](CHANGELOG.md).

## Installing

Download the zip for the latest release from this repository's
[Releases](https://github.com/Inkbraid/casement-plugin-rustdesk/releases) and
unpack it so that you get

```
%LOCALAPPDATA%\Casement\plugins\
    rustdesk\
        plugin.json
        casement-rustdesk.exe
```

then restart Casement. Each release also carries the matching source code
(the tag and GitHub's source archives).

## Addresses

```
rustdesk://<peer-id>?server=<hbbs-host[:21116]>&key=<server public key>[&relay=<hbbr-host[:21117]>]
```

- `peer-id`: the RustDesk ID of the computer to connect to (letters, digits,
  `-` and `_`).
- `server`: your ID server (hbbs). Port 21116 unless given. IPv6 addresses go
  in brackets: `[fd00::5]:21116`.
- `key`: **required**. The server's public key, as in hbbs's
  `id_ed25519.pub` (44 characters of base64, shown in the server's log at
  start). The server is verified against it. It may be pasted as it is (`+`
  stays `+`) or percent-encoded.
- `relay`: your relay server (hbbr). Port 21117 unless given. Without it the
  plugin uses the relay hbbs names.

Example:

```
rustdesk://123456789?server=rd.example.com&key=OeVuKk5nlHiXp+APNn0Y3pC1Iwpwn44JGqrQCsWqmBw=
```

The password comes from Casement (saved, or typed when the plugin reports
that one is needed). The plugin doesn't store it.

### Test pattern

`rustdesk://test` connects to nothing: it draws a moving test pattern and logs
all input to stderr (Casement's log), so Casement's side of the protocol can be
checked without a RustDesk server. Options:

| Option | Effect |
|---|---|
| `width=1280&height=720` | picture size |
| `fps=30` | frame rate |
| `password=…` | report `auth_failed` unless Casement sends this password |
| `fail=…` | report `failed` with this text |
| `close_after=5` | report `closed` after 5 seconds |
| `resize_every=3` | switch between two picture sizes every 3 seconds |

The pointer is echoed back as `cursor_pos`, F1 switches between a cross-hair
cursor and the default arrow, and clipboard text is echoed back once as
`echo: <text>`.

## Building

Rust (stable) is all the skeleton needs:

```
cargo build --release          # target\release\casement-rustdesk.exe
cargo test
```

The Windows build links the C runtime statically (`.cargo/config.toml`).

### Trying it without Casement

`examples/casement-sim.rs` plays Casement's side: it answers surface requests,
acknowledges frames, prints the messages and saves the last picture as a PPM.

```
cargo build && cargo run --example casement-sim -- "rustdesk://test" --seconds 3 --out .
```

Off Windows there are no file-mapping handles to pass, so the simulator and
the tests put each surface in a plain file `surface-<handle>` in the folder
named by `CASEMENT_SURFACE_DIR`, and the plugin maps that instead.

### Logging

The plugin logs to stderr, which Casement copies into its log.
`CASEMENT_RUSTDESK_LOG` takes an [env_logger](https://docs.rs/env_logger)
filter, e.g. `debug`. Nothing but protocol messages is ever written to stdout:
at start the plugin keeps the stdout handle for itself and points the
process's standard output at stderr.

## Licensing

This program is free software under the **GNU Affero General Public License,
version 3** ([LICENSE](LICENSE)).

It reuses code from RustDesk, which is licensed under the AGPL-3.0
(© Purslane Ltd. and contributors). RustDesk's `hbb_common` crate currently
ships no licence file of its own; it is part of RustDesk's AGPL-3.0
workspace and is used under that licence.

Casement itself is a separate program that talks to this plugin only through
the documented protocol in [docs/plugin-protocol.md](docs/plugin-protocol.md);
the plugin's licence does not extend to it.

If you run a modified version for others over a network, the AGPL requires
you to offer them its source. Every release of this plugin is published
together with its source (the release tag and its source archive).
