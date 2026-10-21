> **This is a copy.** The authoritative version of this document lives in
> Casement's own repository. It is reproduced here unchanged so this plugin
> can be built and reviewed against it; changes to the protocol are agreed
> with the Casement side first.

# Casement protocol plugins — version 1

A protocol plugin lets Casement show a kind of remote desktop it doesn't speak itself. The plugin is a separate program: Casement starts it for each tab, tells it where to connect, and from then on the plugin delivers pictures, a cursor and clipboard text while Casement sends it keyboard and mouse input. Everything else — the connection list, saved passwords and Windows Hello, tabs, full screen, screenshots, recording, pop-out windows — stays Casement's.

Plugins and Casement are separate programs that talk only through this protocol. A plugin may be under any licence its author chooses (an AGPL RustDesk plugin, say) without that licence reaching Casement, and Casement works the same with no plugins installed.

## Installing

A plugin is a folder with a manifest, `plugin.json`, and whatever it needs to run:

```
%LOCALAPPDATA%\Casement\plugins\
    rustdesk\
        plugin.json
        casement-rustdesk.exe
```

(`CASEMENT_PLUGINS` names another folder of plugin folders, for tests and portable setups.)

```json
{
  "format": "casement-plugin",
  "version": 1,
  "id": "rustdesk",
  "name": "RustDesk",
  "description": "RustDesk peers through your own ID and relay server",
  "schemes": ["rustdesk"],
  "program": "casement-rustdesk.exe",
  "args": [],
  "homepage": "https://git.example.com/casement-rustdesk"
}
```

- `id`: letters, digits and `-`; unique among installed plugins.
- `schemes`: the address schemes the plugin handles. `rustdesk://…` addresses then open in a tab run by this plugin. Casement's own schemes (`spice`, `vnc`, `rdp`, `ssh`, `telnet`, `serial`, `shell`, `nanokvm`, `glkvm`) can't be taken over.
- `program`: relative to the plugin's folder, or a program on `PATH` (`python`, say); `args` come before anything Casement adds. The program starts in the plugin's folder.

Casement reads the manifests when it starts.

## The connection

Casement starts the program once per tab, with its standard input, output and error redirected:

- **stdin**: messages from Casement.
- **stdout**: messages to Casement. Nothing else may be written there.
- **stderr**: free text; Casement writes each line to its log, prefixed with the plugin's id.

The process belongs to a Windows job object that ends it together with Casement. When the tab disconnects Casement sends `disconnect`, closes stdin and waits two seconds before ending the process.

### Framing

Each message is a 4-byte little-endian length followed by that many bytes of UTF-8 JSON: one object with a `type` member. Messages are at most 16 MiB. Unknown types and unknown members are ignored by both sides, so either side may add to them within a version.

## Messages

### Start

1. Casement → `{"type": "hello", "protocol": 1, "app": "Casement 0.7.1"}`
2. Plugin → `{"type": "hello", "protocol": 1, "name": "RustDesk", "version": "0.1.0", "capabilities": ["clipboard", "cursor"]}` within 10 seconds. A plugin that can't speak the version Casement offered answers with the highest it can; Casement then shows that the plugin needs a newer Casement (or the other way round) and stops.
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

`password` is set when Casement has one: saved, or just typed after the plugin reported `auth_failed`. The plugin must not store it.

Capabilities a plugin may declare:

| Capability | Meaning |
|---|---|
| `clipboard` | text clipboard both ways |
| `cursor` | sends `cursor` and `cursor_pos`; without it Casement shows an arrow and the picture carries the remote cursor |
| `resize` | takes `resize` (the window's size, for remote ends that can follow it) |
| `audio` | plays sound itself and takes `mute` |

### State (plugin → Casement)

`{"type": "state", "state": "connected" | "auth_failed" | "failed" | "closed", "text": "…"}`

- `connected`: logged in. The tab shows the picture once the first `frame` arrives.
- `auth_failed`: the password is missing or wrong. Casement asks the user (offering to save it) and starts a new plugin process with it; this one ends.
- `failed`: couldn't connect; `text` says why, in a sentence for the user.
- `closed`: the remote end closed a working connection.

A plugin that exits without `failed` or `closed` is reported as failed, with its exit code.

### Pictures

Pixels travel through shared memory. Each surface is a Windows file mapping that Casement creates and hands to the plugin.

1. Plugin → `{"type": "surface_request", "width": 1920, "height": 1080}` whenever the remote display's size is known or changes.
2. Casement → `{"type": "surface", "id": 3, "handle": 1234, "width": 1920, "height": 1080, "stride": 7680, "size": 8294400}`. `handle` is a handle in the plugin's own process (Casement duplicated it there), for `MapViewOfFile` with `FILE_MAP_ALL_ACCESS`. Pixels are 32-bit BGRX, top row first, `stride` bytes per row. A surface replaces the one before; the plugin closes the old handle.
3. Plugin → `{"type": "frame", "surface": 3, "rects": [[x, y, w, h], …]}` after drawing: the rectangles that changed, or an empty list for all of it.
4. Casement → `{"type": "frame_ack", "surface": 3}` once it has copied them. Until then the plugin must not write to the surface; decoding the next picture elsewhere meanwhile is fine. One frame is in flight at a time.

### Cursor (plugin → Casement, with `cursor`)

- `{"type": "cursor", "width": 32, "height": 32, "hot_x": 0, "hot_y": 0, "rgba": "<base64>"}`: the pointer's image, 8-bit RGBA, straight alpha, top row first. `{"type": "cursor", "default": true}` means the ordinary arrow.
- `{"type": "cursor_pos", "x": 100, "y": 200, "visible": true}`: where the remote end put the pointer, in surface pixels.

### Input (Casement → plugin)

- `{"type": "key", "scancode": 30, "down": true}`: PC set-1 scancodes, `0x100` added for E0-prefixed keys (right Ctrl `0x11D`, arrows, …), Pause as `0x45` with `0x21D`. Casement sends the keys of Ctrl+Alt+Del and the like as separate key events.
- `{"type": "pointer", "x": 100, "y": 200, "buttons": 1}`: absolute, in surface pixels; `buttons` a mask, 1 left, 2 middle, 4 right.
- `{"type": "wheel", "delta": 1}`: notches, positive away from the user.
- `{"type": "resize", "width": 1600, "height": 900}` (with `resize`).
- `{"type": "mute", "muted": true}` (with `audio`).

### Clipboard (both ways, with `clipboard`)

`{"type": "clipboard", "text": "…"}`: the other side's clipboard changed to this text. Casement sends it only as the user's clipboard settings for the connection allow.

### End

- Casement → `{"type": "disconnect"}`: close the connection and exit.

## Version history

1. First version.
