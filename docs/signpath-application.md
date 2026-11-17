# SignPath Foundation — application notes

Use this as the copy/paste source for https://signpath.org/apply and for the
onboarding configuration after approval.

## Project

- **Name:** Nuuvia
- **Repository:** https://github.com/cesarzxk/nuuvia-vpn
- **Website / download page:** https://nuuvia.devcesar.com/
- **Releases:** https://github.com/cesarzxk/nuuvia-vpn/releases
- **License:** MIT (OSI-approved, no commercial dual licensing)
- **Maintainer:** Cesar — https://github.com/cesarzxk
- **Country:** Brazil

## Short description

Nuuvia is a free, open-source encrypted Layer 2 VPN for Windows and Linux. It
lets Minecraft Java and other LAN games discover each other across different
networks through a shared virtual Ethernet segment, with direct connections when
possible and an encrypted relay fallback. It has no accounts, telemetry, ads, or
paid tier.

## What we want signed

- `nuvia-gui.exe` — Windows GUI
- `nuvia.exe` — CLI
- `lanprobe.exe` — LAN discovery diagnostic
- `nuvia-setup-x86_64.exe` — NSIS installer built with the signed executables
- the portable `.zip` contains the signed executables

The bundled `tap-windows6` driver is an upstream GPLv2 component signed by its
own publisher. It is included in the installer but is not signed with the
Nuuvia/SignPath certificate.

## Build and release process

- Public Rust workspace: `crates/nuvia-core`, `nuvia-cli`, `nuvia-gui`,
  `nuvia-protocol`, `nuvia-server`, `nuvia-probe`.
- GitHub Actions on GitHub-hosted runners builds and tests the workspace.
- Windows binaries are cross-compiled with `x86_64-pc-windows-gnu`; the NSIS
  installer is built on the same runner.
- SignPath will sign the executables; the installer is then rebuilt with the
  signed executables and signed again.
- Releases are created from `v*` tags. Current release: `v1.2.0`.
- Every signing request is manually approved by the maintainer.
- Product metadata is embedded in every PE file: ProductName `Nuuvia`,
  ProductVersion/FileVersion from `Cargo.toml` (currently `1.2.0`).

## Compliance links

- Code signing policy: https://github.com/cesarzxk/nuuvia-vpn/blob/main/CODE_SIGNING.md
- Privacy policy: https://github.com/cesarzxk/nuuvia-vpn/blob/main/PRIVACY.md
- Security policy: https://github.com/cesarzxk/nuuvia-vpn/blob/main/SECURITY.md
- Architecture: https://github.com/cesarzxk/nuuvia-vpn/blob/main/docs/architecture.md

## Roles

- Authors / committers: https://github.com/cesarzxk
- Reviewers: https://github.com/cesarzxk
- Approvers: https://github.com/cesarzxk

MFA is enabled/required for GitHub and will be enabled for SignPath. The project
currently has one maintainer; the policy documents that external pull requests
require review.

## Why SignPath Foundation

Nuuvia is a free MIT project with no commercial edition. The Windows installer
needs a trusted publisher signature so users can verify that the binary was
built from this repository and to reduce the “Unknown publisher” friction. The
project has no budget for a commercial certificate and no hardware token in its
CI environment; SignPath's OSS HSM-backed flow fits the project.

## After approval — onboarding checklist

- [ ] Create the SignPath organization and project `nuuvia`.
- [ ] Create a signing policy `release-signing`.
- [ ] Create an artifact configuration for the Windows executables and installer.
- [ ] Install the SignPath GitHub App on `cesarzxk/nuuvia-vpn`.
- [ ] Add repository secrets/variables:
  - `SIGNPATH_API_TOKEN` (secret)
  - `SIGNPATH_ORG_ID` (variable)
  - `SIGNPATH_PROJECT_SLUG=nuuvia` (variable)
  - `SIGNPATH_POLICY_SLUG=release-signing` (variable)
- [ ] Add the signed-release workflow (manual dispatch first).
- [ ] Build and sign 1.2.0 without changing the version.
- [ ] Replace the Windows assets on the existing `v1.2.0` release:
  - `nuvia-setup-x86_64.exe`
  - `nuvia-windows-x86_64.zip`
- [ ] Verify the signature with `signtool verify /pa /v` on Windows.
- [ ] Update the website/download note from “not signed yet” to the SignPath
  publisher text.

## Notes

- SignPath may decline projects that do not yet show enough public traction.
  If that happens, the fallback is Certum Open Source (€49–69/year).
- Signing does not guarantee the immediate disappearance of every SmartScreen
  warning; it establishes a verified publisher and lets reputation accumulate.
- Keep the version at `1.2.0` for the replacement release, as requested.
