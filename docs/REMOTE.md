# Remote access and phone notifications

Follow and steer ShadowCode from your phone or another computer: read
conversations as they stream, answer approvals, send messages, start tasks,
review changes. The web interface is the same one the desktop window runs,
served by ShadowCode itself. Phone notifications (through ntfy) tell you when
a task needs you.

Remote access is **off by default** and only devices you pair can connect.

## Turn it on

**In the desktop app:** open **Settings › Remote access** and turn on
**Turn on remote access**. It listens on `127.0.0.1:7390` (this computer
only) until you choose another address. Choosing another address or port
restarts the server there (on the same port too: the old one is closed
first).

**Without a window:** run the headless engine with the web server:

```sh
shadowcode serve --remote                              # the saved address (127.0.0.1:7390)
shadowcode serve --remote --remote-address 100.101.102.103:7390   # this run only
```

It prints the address, a one-time pairing link and its QR code. Remote access
runs while the engine does: the desktop window (or `shadowcode serve`) that
owns this profile. Settings are stored per profile.

## Pair a device

1. In **Settings › Remote access**, choose **Pair a device**. (Or run
   `shadowcode remote pair` next to a running desktop or `serve`.)
2. Scan the QR code with the phone's camera, or open the link on the device.
   The link works **once**, for **10 minutes**.
3. The browser stores its own access key and opens ShadowCode. To keep it on
   the home screen, use the browser's *Add to Home Screen* (it installs as an
   app without browser controls).

Paired devices are listed with when they were last used. **Unpair** removes
one device; **Unpair all devices** removes every device and cancels unused
links. From a terminal: `shadowcode remote` (status and devices),
`shadowcode remote revoke <id-prefix>` or `shadowcode remote revoke --all`.

## Reaching it from your phone

The safest set-ups, best first:

1. **Tailscale with HTTPS.** Keep the address on *This computer only* and run

   ```sh
   tailscale serve --bg 7390
   ```

   Tailscale gives this computer an HTTPS address on your tailnet (for
   example `https://your-computer.your-tailnet.ts.net`) and forwards it to
   ShadowCode. Enter that address as **Public address** so pairing links and
   notifications use it. Traffic is encrypted end to end and only your
   tailnet's devices can reach it. `tailscale serve reset` stops it.
2. **Tailscale address.** Choose the *Tailscale* address in **Where it
   listens**. Only devices on your tailnet can connect, and Tailscale encrypts
   the traffic (the page itself is plain HTTP).
3. **Local network.** Choosing a local network address (or *Every network*)
   makes ShadowCode reachable from other devices on that network over **plain
   HTTP, which is not encrypted**: anyone on the same network can read what
   you see and send, including the access key. Use it only on a network you
   trust, and prefer Tailscale.

ShadowCode never opens ports on your router and never contacts a relay
service.

## What a remote device can do

The same as the desktop window, with these exceptions:

- **Terminals are off.** Interactive terminals, **Run** (a command typed
  in the drawer) and the `/run`, `/test <command>` and `/background` slash
  commands are refused unless you turn on **Allow terminals over remote
  access**. Tasks still run commands through the usual approvals, which the
  device can answer: a paired device can still get commands run, so pair only
  devices you control.
- **No app preview.** The drawer's Preview tab loads your dev server through
  proxies on this computer's loopback, which a remote device cannot reach;
  `/api/preview…` is refused and the tab says so.
- **Remote access settings stay on this computer.** A remote device cannot
  see or change pairing, devices, the address or phone notifications.
- **Your data stays on this computer.** Backups, restore, repair and reset
  (Settings › Your data, `/api/data…`) are refused: a backup can hold API
  keys, and a restore or reset replaces everything.
- **No secrets.** Secret files (`.env`, `secrets.env`, private keys,
  credential JSON), also when reached through a symlink or a spelling such as
  `.env/`, are not shown or diffed, ShadowCode's own settings folder
  cannot be opened as a project, and recognizable keys and tokens in any
  answer are replaced by `[redacted secret]`. Text containing a redacted
  value cannot be saved back from a remote device.
- **Its own navigation.** Opening a project or conversation on the phone does
  not change what the desktop window shows. Each browser tab keeps its own.
- **No file pickers.** Type a folder path to open a project; exports download
  in the browser.

## Phone notifications (ntfy)

[ntfy](https://ntfy.sh) delivers push notifications to its Android and iOS
apps. ShadowCode sends nothing until you enter a server and a topic.

1. Install the ntfy app and subscribe to a topic. Use a long, random topic:
   on a public server anyone who knows the name can read its messages. The
   **Generate** button makes one.
2. In **Settings › Remote access › Phone notifications**, enter the server
   (`https://ntfy.sh`, or your own) and the topic, and an access token if
   your server needs one (it is stored with your other secrets and never
   shown again). **Save**, then **Send a test**.
3. Choose which events notify you: *needs my approval*, *finishes*, *fails*,
   *reaches a plan limit*. These follow the same rules as desktop
   notifications (cancelled tasks never notify).

Messages name the project and what happened. The approval request or task
summary is included only if you turn on **Include task details**; keys in it
are redacted. When remote access is running (or a public address is set),
tapping the notification opens the conversation in the web interface.
Offline mode stops phone notifications. At most 20 messages are sent in 10
minutes.

## Security details

- **Tokens.** Each paired device gets its own 256-bit random access key.
  ShadowCode stores only its SHA-256 digest, in `remote.json` in the profile's
  config folder (mode 600), and compares digests in constant time. The key
  lives in the browser's local storage for this site and travels only in the
  `Authorization` header, never in a URL or a cookie. Pairing codes are
  256-bit, single-use and expire after 10 minutes; they travel in the link's
  `#fragment`, which browsers do not send to servers, and the web interface
  removes it from the address bar at once.
- **Failed attempts.** After 8 failed token or pairing attempts from one
  address within 5 minutes, that address is refused for 5 minutes.
- **Browsers.** There are no CORS headers: cross-origin requests and
  preflights are refused, and a request whose `Origin` is not this server's
  own is rejected. The server answers only its own host names (IP addresses,
  `localhost`, `*.localhost`, Tailscale `*.ts.net` names and the public
  address), so a web page that points its own domain at this computer (DNS
  rebinding) gets `421` before any token is checked and cannot use up the
  failed-attempt budget. If you reach the server by another name, enter that
  address as the **Public address**. Pages are served with a strict Content Security Policy,
  `X-Frame-Options: DENY` and `nosniff`.
- **Limits.** Request bodies up to 8 MB (pairing: 4 KB), 64 connections, 32
  event streams, 32 browser tabs per server; static files are served only
  from the built interface by exact name, never from the disk by path.

## For developers

The server is `native/core/src/remote/` (see the module comments). Routes:
`GET /`, `/assets/*`, `/manifest.webmanifest`, icons (the built UI, no
token); `POST /_remote/pair`; `GET /_remote/session`; `GET /_remote/stream`
(Server-Sent Events, the same wake-ups the desktop shell forwards:
`shadowcode:events {session_id, type}` and, when terminals are allowed,
`shadowcode:terminal {type, terminal_id}`); `/api/*` (the application API,
filtered by `remote::policy`). Management routes are `/api/remote*`; see
[API_CONTRACT.md](API_CONTRACT.md#remote-access). The UI picks its
transport in `ui/src/lib/transport.ts`: Tauri IPC in the desktop, HTTP
(`ui/src/lib/remote.ts`) in a browser.
