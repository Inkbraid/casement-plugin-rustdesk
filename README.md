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
| RustDesk peers via a self-hosted hbbs/hbbr: relayed connection, server-key verification, password login | done |
| Video: VP9, VP8, AV1 | done |
| Keyboard (Map mode), mouse buttons and wheel, cursor shape and position, text clipboard both ways | done |
| H.264/H.265 (needs RustDesk's hardware codec build) | not yet; peers fall back to VP9 |
| Hole punching / direct LAN, audio, `resize`, multiple displays, two-factor login | later |

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
that one is needed). The plugin doesn't store it. The peer needs a password
(RustDesk's permanent password, or its current one-time password); accepting
a session by click on the remote computer without one isn't supported.

### How a connection is made

1. The plugin asks your hbbs for the peer, always requesting a relay (hole
   punching and direct connections come later), and hbbs pairs both sides
   on your hbbr.
2. hbbs vouches for the peer's long-term key by signing it with the server
   key, and the peer signs a fresh session key with its own. Both signatures
   are checked against `key` from the address; if either doesn't verify, the
   plugin refuses to connect. (RustDesk's own client falls back to an
   unencrypted connection in that case; this plugin doesn't.)
3. The session is encrypted with RustDesk's current key exchange (version 1,
   with the older version 0 for older peers), then the plugin logs in with the
   password and the picture starts.

What Casement sees as `state`:

| | |
|---|---|
| `connected` | logged in |
| `auth_failed` | no password, or a wrong one |
| `failed` | the ID isn't known, the peer is offline, the key doesn't match, a server can't be reached, the peer can't be verified, a two-factor code is required, … with a sentence saying which |
| `closed` | the peer ended a working session, or stopped answering for 30 seconds |

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

## RustDesk's code in this plugin

The plugin reuses RustDesk's own code rather than reimplementing its
protocol, all from [rustdesk/rustdesk](https://github.com/rustdesk/rustdesk)
at commit **`c7f212df2c960fa74bd83d3849a5a7ce7809a047`** (2026-10-20,
version 1.5.1 in development):

- as git dependencies at that commit: `hbb_common` (rendezvous protocol,
  framing, encryption and key exchange), `base` (the message protocol) and
  `scrap` (the VP8/VP9/AV1 decoders); `rdev` from rustdesk-org at the
  revision RustDesk's lockfile uses, for its key code tables;
- as code adapted into `src/rustdesk/`, each file naming what it was adapted
  from: the client's connection set-up (`src/client.rs`), handshake helpers
  (`src/common.rs`), message handling (`src/client/io_loop.rs`), Map-mode
  keys (`src/keyboard.rs`), clipboard messages (`src/clipboard.rs`);
- vcpkg overlay ports for the codec libraries (`vcpkg-ports/`).

RustDesk's client lives in private modules of its main crate (`librustdesk`)
and is reachable from outside only through the Flutter FFI, so driving it
through its UI trait (`InvokeUiSession`) would need a fork; the main crate also
brings the whole RustDesk server, tray, updater, Sciter/Flutter glue and audio.
Adapting the few client files that a remote view needs pulls in far less.

To move to a newer RustDesk commit: update the `rev` of the three crates in
`Cargo.toml` and `RUSTDESK_COMMIT`/`RUSTDESK_VERSION` in
`src/rustdesk/mod.rs`, compare the adapted functions with their upstream
versions, copy `res/vcpkg/{aom,libvpx,libyuv}` again and match the vcpkg
baseline in `vcpkg.json` and the CI workflows.

## Building

The deliverable is a Windows x64 program; the tests also run on Linux.

### Windows

Needs Rust (stable, MSVC), LLVM (for bindgen) and vcpkg for the codec
libraries:

```
vcpkg install --triplet x64-windows-static --x-install-root=%VCPKG_ROOT%\installed
cargo build --release          # target\release\casement-rustdesk.exe
```

`vcpkg.json` and `vcpkg-ports/` build libvpx, aom and libyuv the way
RustDesk's `scrap` crate links them. The C runtime is linked statically
(`.cargo/config.toml`), so the program needs nothing else installed. It is
built for the Windows GUI subsystem, so starting it never opens a console
window; its redirected standard handles work as usual.
`.github/workflows/ci.yml` does exactly this.

### Linux (tests)

The codec libraries come from the distribution. On Ubuntu:

```
sudo apt-get install libvpx-dev libaom-dev libyuv-dev libjpeg-dev libx11-dev libxi-dev \
    libxtst-dev libxext-dev libxfixes-dev libxcb1-dev libxcb-shm0-dev libxcb-randr0-dev \
    libclang-dev pkg-config
PKG_CONFIG_PATH=$PWD/ci/pkgconfig cargo test
```

(`ci/pkgconfig/libyuv.pc` stands in for the pkg-config file Ubuntu's libyuv
lacks.)

## Testing

- `cargo test` runs unit tests and `tests/stdio.rs`, which plays Casement's
  side over stdin/stdout against the built program (with real file mappings
  on Windows).
- `tests/e2e.rs` tests against a real RustDesk peer: `tools/e2e/up.sh` starts
  hbbs and hbbr (`rustdesk/rustdesk-server`) and the official RustDesk 1.5.0
  Linux build under Xvfb in Docker, then

  ```
  . tools/e2e/env && cargo test --test e2e -- --nocapture
  ```

  checks the picture, pointer position, typing, clipboard both ways,
  `auth_failed` and a wrong server key. CI runs it on every pull request.

### Trying it without Casement

`examples/casement-sim.rs` plays Casement's side: it answers surface requests,
acknowledges frames, prints the messages and saves the last picture as a PPM.

```
cargo build && cargo run --example casement-sim -- "rustdesk://test" --seconds 3 --out .
cargo run --example casement-sim -- "rustdesk://<id>?server=…&key=…" --password … \
    --click 600,400 --type 0x23,0x17,0x1C --clipboard "hello"
```

Off Windows there are no file-mapping handles to pass, so the simulator and
the tests put each surface in a plain file `surface-<handle>` in the folder
named by `CASEMENT_SURFACE_DIR`, and the plugin maps that instead.

### Logging and troubleshooting

The plugin logs to stderr, which Casement copies into its log.
`CASEMENT_RUSTDESK_LOG` takes an [env_logger](https://docs.rs/env_logger)
filter, e.g. `debug`. Nothing but protocol messages is ever written to stdout:
at start the plugin keeps the stdout handle for itself and points the
process's standard output at stderr.

RustDesk's crates keep settings in a folder named after the program; the
plugin names it `casement-rustdesk`, so a RustDesk installed on the same
computer is neither read nor changed, and the plugin writes no settings of
its own.

If a connection fails, the log says at which step: the ID server, the relay
(`joining relay …`), the handshake (`secured with …`) or the login.

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
