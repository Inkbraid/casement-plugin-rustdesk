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
- `plugin.json`, protocol document, `casement-sim` example, integration tests
  over stdio, GitHub Actions (Windows build and tests, Linux tests, releases
  from tags).
