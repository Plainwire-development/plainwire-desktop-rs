# plainwire-desktop

a native desktop client for [Plainwire](https://plainwi.re), written in rust with (MY FAVORITE LANGUAGE I LOVE IT AAYAYYAAAHHHJBHJBHJBDVH)
[egui](https://github.com/emilk/egui). and ts is not some weird ahhh tauri thing ITS A DAMN NATIVE APP

> beta sawdly 3;. it talks to a real server and the everyday paths are tested, but WebRTC and
> screen sharing have never been exercised against a second peer — see
> known limitations.

## What it does

**messaging (definetaly the most useless part)**
- direct messages and server channels, with unread counts and a live connection dot
- message history with *load older messages* paging
- typing indicators, live message/delete/reaction updates over the WebSocket
- click any existing reaction chip to add or remove your own
- pasting or dropping images and files
- profile cards for any user, with relationship status
- basically has everything u need

**servers**
- server list, channel tree, text and voice channels, join/leave from the sidebar
- people panel with name/username search and one-click DMs

**voice and video**
- Voice channels over a WebRTC mesh, with mic capture through `cpal`
- Mute, deafen, and per-participant speaking indicators
- Screen sharing, captured through `ffmpeg`
- Direct calls: ring, accept, decline, cancel
> might have issues still

**session**
- multiple accounts on one instance, remembered between launches
- session token reuse, so a restart does not need your password again

## requirements

- rust 1.85 or newer (the crate uses edition 2024)
- a gpu backend supported by wgpu — the app requests `eframe::Renderer::Wgpu`
- linux only for voice input: `alsa-sys` links against ALSA through `pkg-config`
- optional, at runtime:
  - `ffmpeg` for screen sharing
  - `xdg-open` for opening attachments in your browser
  - x11 or wayland session libraries; the wayland clipboard path
    (`arboard`'s `wayland-data-control` feature) needs a compositor that implements
    the data-control protocol

on debian and debian-based:

```sh
sudo apt install build-essential pkg-config libasound2-dev
```

macOS and windows are untested; the audio and clipboard backends are wired for
linux, so other platforms may not build or work. please do not report issues with those.

## build and run

```sh
cargo build --release
./target/release/plainwire-desktop
```

for development:

```sh
cargo run
cargo test
```

The window opens at 1160×760 with a 720×480 minimum.

to get a tray icon, put a 32×32 RGBA image in an `assets/` directory one level
above the binary — the app reads `<exe-dir>/../assets`, which is `target/assets`
for a normal `cargo run`. It picks up the first file it finds there. im lwk lazy

## using it

on the login screen, enter your instance URL,
username, and password. Connecting stores the session, so later launches resume
straight into the app. Use the account menu in the sidebar to add accounts, switch
between them, or sign out.

in the composer:

| action | how |
| --- | --- |
| send | `Enter`, or the **Send** button |
| attach the clipboard image or file | `Ctrl+Shift+V`, or the **📎** button |
| attach a file | drag it onto the window |
| apload result | the image markup lands in the composer; press `Enter` to send it |

`ctrl+V` stays with egui and pastes text. `ctrl+Shift+V` is the attachment
shortcut, because egui-winit consumes plain `ctrl+V` and only reports a paste when
the clipboard contains text — which is why an image paste would otherwise do
nothing at all.

## how it works

```
src/
  main.rs      window, tokio runtime, wgpu renderer
  app.rs       all UI and input handling
  backend.rs   command/update plumbing and the worker loop
  api.rs       reqwest client, envelope unwrapping, uploads
  realtime.rs  WebSocket connection and event decoding
  model.rs     serde types and the message-body parser
  media.rs     WebRTC mesh, cpal microphone, ffmpeg screen capture
  account.rs   stored accounts
  tray.rs      tray icon
```

egui runs on the main thread, outside the tokio runtime, so every background task
is spawned through `self.rt.spawn`. The UI never blocks: it sends a `Command` to
the worker, which performs the request and sends back an `Update` that `App::apply`
consumes. Both channels are unbounded, so a slow request stalls nothing.

every HTTP call carries the `pw_session` cookie and the `X-CSRF-Token` header.
uploads are raw-body `POST /api/uploads` requests with a percent-encoded
`X-File-Name` and an exact `Content-Length`, matching what the server expects —
not multipart. Images are fetched through the same authenticated client rather
than egui's HTTP loader, because `/api/files/*` and `/api/media/*` are
cookie-gated.

message bodies are parsed for `![alt](url)` and `[text](url)` so attachments
render as images and links instead of raw markup.

## configuration

stored accounts live in `~/.config/plainwire` (or the platform equivalent):

```json
{
  "accounts": [
    {
      "base": "https://plainwi.re",
      "username": "you",
      "display_name": "You",
      "user_id": 42,
      "avatar_url": "/api/media/…",
      "token": "…",
      "csrf": "…"
    }
  ],
  "selected": 0
}
```

this file holds a live session token and CSRF token **in plaintext**. Anyone who
can read it is you, so it stays in your user account with normal permissions.

sign out from the account menu to drop the stored session.

## known limitations sadly

- webRTC, calls, and screen sharing have never been tested against a real second
  peer.
- screen capture shells out to `ffmpeg` with `x11grab`, so it needs `DISPLAY` and
  does not work on Wayland.
- pasting images is not resized or compressed client side, so a large screenshot
  uploads at full size and can hit the server's upload limit.
- opening a full-size attachment assumes `xdg-open` and fails silently if it is
  missing.
- avatars and inline images share one texture cache keyed by URL, capped at three
  fetch attempts before a URL is written off.
- no notifications, no offline queue: messages you send while disconnected are not
  retried.

## Tests

```sh
cargo test
```
## License

AGPL-3.0. See [LICENSE](LICENSE).