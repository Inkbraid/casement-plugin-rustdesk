# Changelog

## 0.2.0 (2026-10-23)

- Sound: the peer's sound plays on Windows' default output device. The
  login asks for sound instead of turning it off; the Opus packets are
  decoded with libopus (through RustDesk's `magnum-opus` binding, libopus
  built by vcpkg with RustDesk's port) and played through WASAPI in shared
  mode, following the default device when it changes. A jitter buffer
  starts the sound 60 ms behind and grows the delay when sound comes late;
  RustDesk on Linux sends about two seconds at a time, which plays without
  gaps.
- Capability `audio` in `plugin.json` and `hello`, and `mute`: muting stops
  the sound at once and asks the peer to stop sending it, as RustDesk's own
  client does; unmuting asks for it again. A tab muted before the
  connection is made logs in without sound.
- The release zip carries the licences of the libraries built into the
  program (libvpx, aom, libyuv, libopus) in `licenses/`.
- Tests: the end-to-end peer plays a tone through PulseAudio, which has to
  arrive, play, stop when muted and come back when unmuted;
  `tools/sound-check` plays tones through the WASAPI output under Wine and
  checks the recording. Both run in CI.

## 0.1.0 (2026-10-23)

- Casement plugin protocol v1: length-prefixed JSON framing, `hello`,
  `connect`, `state`, `disconnect`, shared-memory surfaces with
  `frame`/`frame_ack` flow control, `cursor`, `cursor_pos`, key, pointer and
  wheel input, clipboard text both ways. Capabilities: `password`,
  `clipboard`, `cursor`.
- `rustdesk://test`: a moving test pattern that logs input, for testing
  Casement's side end to end.
- Address parsing for `rustdesk://<id>?server=…&key=…[&relay=…]`.
- stdout carries protocol messages only; anything else printed goes to stderr.
- RustDesk peers through a self-hosted hbbs/hbbr (`rustdesk://<id>?server=…&key=…[&relay=…]`):
  relayed connection, peer verified against the server key (refusing rather
  than falling back to an unencrypted connection), versioned key exchange,
  password login (`auth_failed` for a missing or wrong password), VP9/VP8/AV1
  video with dirty rectangles, keyboard in Map mode, mouse buttons and wheel,
  cursor shape and position, text clipboard both ways, `closed`/`failed`
  with a sentence for the user. Reuses RustDesk's `hbb_common`, `base` and
  `scrap` and adapted client code from RustDesk at
  `c7f212df2c960fa74bd83d3849a5a7ce7809a047`.
- `plugin.json` with the connection fields (peer ID, ID server, key, relay)
  for Casement's connection dialog, an icon, the protocol document, the
  `casement-sim` example, integration tests over stdio, an end-to-end test
  against RustDesk 1.5.0 in Docker (`tools/e2e`), GitHub Actions (Windows
  build and tests, Linux tests and end-to-end test, releases from tags).
