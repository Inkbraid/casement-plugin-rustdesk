# Changelog

## 0.1.0 (unreleased)

- Casement plugin protocol v1: length-prefixed JSON framing, `hello`,
  `connect`, `state`, `disconnect`, shared-memory surfaces with
  `frame`/`frame_ack` flow control, `cursor`, `cursor_pos`, key, pointer and
  wheel input, clipboard text both ways. Capabilities: `clipboard`, `cursor`.
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
- `plugin.json`, protocol document, `casement-sim` example, integration tests
  over stdio, an end-to-end test against RustDesk 1.5.0 in Docker
  (`tools/e2e`), GitHub Actions (Windows build and tests, Linux tests and
  end-to-end test, releases from tags).
