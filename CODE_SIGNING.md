# Code signing policy

Free code signing provided by [SignPath.io](https://about.signpath.io), certificate by [SignPath Foundation](https://signpath.org).

Nuuvia uses SignPath Foundation for Windows code signing. A Windows release is
published only after its SignPath signing request has been reviewed and approved
by the project approver.

## What is signed

The following Windows artifacts are submitted for signing:

- `nuvia-gui.exe`
- `nuvia.exe`
- `lanprobe.exe`
- `nuvia-setup-x86_64.exe`
- the portable `.zip` contains the signed executables

The bundled `tap-windows6` driver is an upstream GPLv2 component signed by its
own publisher. It is included in the installer but is **not** signed with the
Nuuvia/SignPath certificate.

The optional software renderer in `soft\` bundles an unmodified Mesa3D 26.3.0
build (MIT-licensed, from `pal1000/mesa-dist-win`) as `opengl32.dll` and
`libgallium_wgl.dll`. These upstream DLLs are **not** signed with the
Nuuvia/SignPath certificate; their license is included as
`soft\MESA-LICENSE.txt`.

## Release process

1. A release is built from this public repository by GitHub Actions on a
   GitHub-hosted runner.
2. The Windows executables are signed through SignPath.
3. The NSIS installer is rebuilt with the signed executables and then signed.
4. The signed installer and portable zip are attached to the GitHub release.
5. Every signing request requires manual approval in SignPath; no release is
   signed automatically without that approval.

Only artifacts built from this repository are signed. No upstream binaries are
signed with the Nuuvia certificate.

## Roles

- **Authors / committers:** [@cesarzxk](https://github.com/cesarzxk)
- **Reviewers:** [@cesarzxk](https://github.com/cesarzxk) — external pull
  requests require review before merge.
- **Approvers:** [@cesarzxk](https://github.com/cesarzxk) — every signing
  request must be approved manually.

Multi-factor authentication is required for GitHub and SignPath access. The
project currently has a single maintainer; new maintainers must be added to the
roles above before they can author, review, or approve releases.

## Version metadata

All signed PE files use the same product metadata, generated in `build.rs` and
the NSIS script:

- **ProductName:** Nuuvia
- **ProductVersion / FileVersion:** the workspace version in `Cargo.toml`
  (currently `1.2.4`)
- **CompanyName:** Nuuvia contributors
- **LegalCopyright:** MIT License

## Privacy

See [PRIVACY.md](PRIVACY.md). The application does not include telemetry or
analytics.

## Installation and uninstallation

The Windows installer registers an uninstaller in **Apps & Features** and
creates Start Menu/Desktop shortcuts. Uninstalling removes the application,
shortcuts, installed driver entry and firewall rules created by the installer.
Manual removal instructions are in [deploy/windows/README.md](deploy/windows/README.md).

## Download pages

- Website: https://nuuvia.devcesar.com/
- Releases: https://github.com/cesarzxk/nuuvia-vpn/releases
- Source: https://github.com/cesarzxk/nuuvia-vpn

## Verification

After a release is signed, Windows shows the verified publisher
**SignPath Foundation** in the UAC prompt and file properties. SignPath also
records the source repository, workflow and build context for each signing
request.
