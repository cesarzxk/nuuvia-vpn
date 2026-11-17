# Privacy policy

Nuuvia does not include telemetry, analytics, advertising, or crash reporting.
The application does not create an account and does not send usage data to the
project maintainers.

## Data sent over the network

To connect players, Nuuvia contacts the configured rendezvous/relay server
(currently `https://167.234.237.6.sslip.io`, UDP port `8443`). The rendezvous
server receives:

- the opaque room ID derived from the room name;
- the peer public key and a short peer tag;
- the relay token derived from the room password;
- the public UDP endpoint observed by the server;
- LAN endpoint hints advertised by the client;
- the optional device display name (hostname).

The server never receives the room password or the traffic keys. Relay traffic
is end-to-end encrypted; the relay forwards ciphertext and cannot decrypt it.

The app may also connect directly to other peers on the LAN or on the internet,
using the endpoints exchanged through the rendezvous server. Direct traffic is
encrypted with the same room group key.

## Data stored locally

- `identity.toml` — private/public X25519 identity, mode `0600`.
- `rooms.toml` — saved room names and passwords, mode `0600`.
- `nuvia.log` — local diagnostic log on Windows.

These files stay on the user's machine. Removing a room from the app does not
delete the remote relay state; the relay keeps rooms in memory until restart or
expiry.

## Website

The Nuuvia website is a static GitHub Pages site. It does not use cookies,
analytics, or third-party tracking scripts. GitHub may process standard access
logs according to the [GitHub Privacy Statement](https://docs.github.com/en/site-policy/privacy-policies/github-privacy-statement).

## Third-party components and services

- **GitHub** hosts the source code, releases, and the static website.
- **OpenVPN tap-windows6** is the GPLv2 TAP driver bundled with the Windows
  installer. It does not contain telemetry.
- **ElevenLabs** was used only to produce the promotional video narration. It is
  not part of the application runtime.

## Contact

For privacy questions, open an issue at
https://github.com/cesarzxk/nuuvia-vpn/issues. For security-sensitive reports,
use the private advisory flow described in [SECURITY.md](SECURITY.md).

Last updated: 2026-11-17.
