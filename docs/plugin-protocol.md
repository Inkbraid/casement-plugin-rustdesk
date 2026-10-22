# Casement plugin protocol, version 1

> A copy of the protocol from
> [Inkbraid/casement-plugin-protocol](https://github.com/Inkbraid/casement-plugin-protocol),
> licensed CC BY 4.0. The authoritative copy is there.

A protocol plugin adds a connection type that Casement doesn't support
itself. The plugin is a separate program. Casement starts it for each
connection and tells it where to connect. The plugin sends screen contents,
cursor and clipboard text; Casement sends keyboard and mouse input.

Connection list, saved passwords, tabs, full screen, screenshots, recording
and pop-out windows are implemented in Casement.

Plugins and Casement are separate programs that communicate only through
this protocol. A plugin can use any licence, and Casement works the same
without any plugins installed.

[`examples/mock-plugin/`](https://github.com/Inkbraid/casement-plugin-protocol/tree/main/examples/mock-plugin) is a complete plugin in
Python. It's used by Casement's tests and is a good starting point.

## Installation

A plugin is a folder containing a manifest (`plugin.json`), an icon and
whatever the plugin needs to run:

```
%LOCALAPPDATA%\Casement\plugins\
    rustdesk\
        plugin.json
        icon.png
        casement-rustdesk.exe
```

The environment variable `CASEMENT_PLUGINS` can point to a different
plugins folder, for tests and portable setups. Casement reads the manifests
at startup.

## Manifest

```json
{
  "format": "casement-plugin",
  "version": 1,
  "id": "rustdesk",
  "name": "RustDesk",
  "description": "RustDesk through a self-hosted ID and relay server",
  "icon": "icon.png",
  "program": "casement-rustdesk.exe",
  "args": [],
  "homepage": "https://github.com/Inkbraid/casement-plugin-rustdesk",
  "capabilities": ["password", "clipboard", "cursor"],
  "connections": [
    {
      "scheme": "rustdesk",
      "name": "RustDesk (self-hosted)",
      "fields": [
        { "name": "peer", "label": "Peer ID", "part": "host", "type": "text", "required": true, "placeholder": "123456789" },
        { "name": "server", "label": "ID server", "part": "query", "type": "text", "required": true, "placeholder": "hbbs.example.com:21116" },
        { "name": "key", "label": "Server key", "part": "query", "type": "text", "required": true },
        { "name": "relay", "label": "Relay server", "part": "query", "type": "text", "placeholder": "hbbr.example.com:21117" }
      ]
    }
  ]
}
```

| Member | Required | Description |
|---|---|---|
| `format` | yes | Always `casement-plugin`. |
| `version` | yes | Manifest version, `1`. |
| `id` | yes | Letters, digits and `-`. Unique among installed plugins. |
| `name` | yes | Shown in Casement's plugin list. |
| `description` | no | One line, shown in the plugin list. |
| `icon` | no | PNG file in the plugin folder. See [Icon](#icon). |
| `program` | yes | The program to start: a path relative to the plugin folder, or a program on `PATH` (for example `python`). It runs with the plugin folder as its working directory. |
| `args` | no | Arguments, passed before anything Casement adds. |
| `homepage` | no | Where users find updates. |
| `capabilities` | no | What the plugin supports. See [Capabilities](#capabilities). |
| `connections` | yes | The connection types the plugin provides. See [Connection types](#connection-types). |

### Icon

A square PNG with a transparent background, at least 64×64 pixels (256×256
recommended). Casement shows it in the connection list, on tabs and in the
New connection dialog, scaled to the display. Without an icon Casement uses
a generic plugin icon.

### Capabilities

| Capability | Meaning |
|---|---|
| `password` | Connections may need a password. Casement shows the password field and can save the password, optionally protected by Windows Hello. |
| `clipboard` | Clipboard text in both directions. Casement shows its clipboard settings for the connection. |
| `cursor` | The plugin sends the remote cursor (`cursor`, `cursor_pos`). Without it, Casement shows an arrow and the screen contents include the remote cursor. |
| `resize` | The remote screen can follow the window size (`resize`). |
| `audio` | The plugin plays sound itself and handles `mute`. |

The manifest lists the capabilities so Casement can show the right settings
before the plugin runs. The plugin confirms them in its `hello` message;
Casement uses only capabilities listed in both.

### Connection types

Each entry in `connections` is one connection type:

| Member | Required | Description |
|---|---|---|
| `scheme` | yes | The address scheme, for example `rustdesk` for `rustdesk://…` addresses. Lower case. Casement's own schemes (`spice`, `spice+tls`, `vnc`, `rdp`, `ssh`, `telnet`, `serial`, `shell`, `nanokvm`, `glkvm`, `http`, `https`) can't be used. |
| `name` | yes | Shown in the New connection dialog. |
| `icon` | no | A PNG for this connection type, if it differs from the plugin's icon. |
| `fields` | yes | The values that make up an address, in the order the dialog shows them. |

Casement builds its connection dialog from the fields, assembles the
address from what the user enters, and splits an existing address back into
the fields for editing. Users can also type an address directly.

Each field:

| Member | Required | Description |
|---|---|---|
| `name` | yes | Letters, digits and `_`. For `query` fields this is the parameter name. |
| `label` | yes | Shown next to the field. |
| `part` | yes | Where the value goes in the address: `user`, `host`, `port`, `path` or `query`. |
| `type` | no | `text` (default), `number`, `choice` or `flag`. |
| `required` | no | `true` if the address isn't valid without it. Default `false`. |
| `default` | no | The initial value. |
| `placeholder` | no | Example text shown in an empty field. |
| `choices` | no | For `choice`: a list of `{ "value": "…", "label": "…" }`. |

An address is put together as:

```
<scheme>://[<user>@]<host>[:<port>][/<path>][?<query>]
```

- At most one field each may use `user`, `host`, `port` and `path`.
  Exactly one field must use `host`.
- `query` fields appear as `name=value`, in the order they are listed,
  joined by `&`. Empty optional fields are left out. A `flag` field appears
  as `name` when on and is left out when off.
- Values are percent-encoded (RFC 3986). IPv6 addresses in `host` go in
  square brackets.

Passwords never go in an address. A plugin that needs one declares the
`password` capability and receives it in `connect`.

## Process and streams

Casement starts the program once per connection, with standard input,
output and error redirected:

- **stdin**: messages from Casement.
- **stdout**: messages to Casement. Nothing else may be written there.
- **stderr**: free text. Casement writes each line to its log, prefixed
  with the plugin's `id`.

The process is part of a Windows job object, so it ends when Casement ends.
When the user disconnects, Casement sends `disconnect`, closes stdin and
ends the process if it is still running two seconds later.

### Framing

Each message is a 4-byte little-endian length followed by that many bytes of
UTF-8 JSON: one object with a `type` member. Messages are at most 16 MiB.
Both sides ignore unknown message types and unknown members.

## Messages

### Start

1. Casement → `{"type": "hello", "protocol": 1, "app": "Casement 0.7.0"}`
2. Plugin → `{"type": "hello", "protocol": 1, "name": "RustDesk", "version": "0.1.0", "capabilities": ["password", "clipboard", "cursor"]}`,
   within 10 seconds. If the plugin doesn't support the version Casement
   sent, it answers with the highest version it supports, and Casement
   reports that the plugin or Casement needs an update.
3. Casement → `connect`:

```json
{
  "type": "connect",
  "address": "rustdesk://123456789?server=hbbs.example.com&key=…",
  "username": "",
  "password": "",
  "title": "Office PC"
}
```

`username` is the address's `user` part, if any. `password` is set when
Casement has one, either saved or just entered after an `auth_failed`. The
plugin must not store it.

### State (plugin → Casement)

`{"type": "state", "state": "connected" | "auth_failed" | "failed" | "closed", "text": "…"}`

- `connected`: logged in. Casement shows the screen when the first `frame`
  arrives.
- `auth_failed`: the password is missing or wrong. Casement asks the user
  for it (offering to save it) and starts a new plugin process. This
  process should exit.
- `failed`: the connection couldn't be made. `text` explains why, in a
  sentence for the user.
- `closed`: the remote side closed a working connection.

If the plugin exits without sending `failed` or `closed`, Casement reports
the connection as failed, with the exit code.

### Screen contents

Screen contents are passed through shared memory. Casement creates a
Windows file mapping (a surface) and gives it to the plugin.

1. Plugin → `{"type": "surface_request", "width": 1920, "height": 1080}`
   when the remote screen size is known or changes.
2. Casement → `{"type": "surface", "id": 3, "handle": 1234, "width": 1920, "height": 1080, "stride": 7680, "size": 8294400}`.
   `handle` is valid in the plugin's process (Casement duplicates it there)
   and is opened with `MapViewOfFile` and `FILE_MAP_ALL_ACCESS`. Pixels are
   32-bit BGRX, top row first, `stride` bytes per row. A new surface
   replaces the previous one; the plugin closes the old handle.
3. Plugin → `{"type": "frame", "surface": 3, "rects": [[x, y, w, h], …]}`
   after drawing. `rects` lists the changed areas; an empty list means the
   whole surface.
4. Casement → `{"type": "frame_ack", "surface": 3}` after copying the
   changes. The plugin must not write to the surface until then. It can
   decode the next frame elsewhere in the meantime. Only one frame is in
   flight at a time.

### Cursor (plugin → Casement, with `cursor`)

- `{"type": "cursor", "width": 32, "height": 32, "hot_x": 0, "hot_y": 0, "rgba": "<base64>"}`:
  the cursor image, 8-bit RGBA, straight alpha, top row first.
  `{"type": "cursor", "default": true}` means the standard arrow.
- `{"type": "cursor_pos", "x": 100, "y": 200, "visible": true}`: the
  remote cursor position, in surface pixels.

### Input (Casement → plugin)

- `{"type": "key", "scancode": 30, "down": true}`: PC set-1 scancodes.
  E0-prefixed keys have `0x100` added (right Ctrl is `0x11D`, the arrows
  `0x148` and so on). Pause is sent as `0x21D` followed by `0x45`. Key
  combinations such as Ctrl+Alt+Del arrive as separate key events.
- `{"type": "pointer", "x": 100, "y": 200, "buttons": 1}`: absolute
  position in surface pixels. `buttons` is a mask: 1 left, 2 middle,
  4 right.
- `{"type": "wheel", "delta": 1}`: wheel notches, positive away from the
  user.
- `{"type": "resize", "width": 1600, "height": 900}`: with `resize`.
- `{"type": "mute", "muted": true}`: with `audio`.

### Clipboard (both directions, with `clipboard`)

`{"type": "clipboard", "text": "…"}`: the clipboard text on the sending side
changed. Casement only sends and accepts clipboard text as the
connection's clipboard setting allows.

### End

- Casement → `{"type": "disconnect"}`: close the connection and exit.

## Version history

1. First version.
