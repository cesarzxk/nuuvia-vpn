# Security policy

## Supported versions

Security fixes are provided for the current `1.2.x` release line.

## Reporting a vulnerability

Please **do not** open a public issue for a vulnerability.

Use GitHub's private vulnerability reporting:

https://github.com/cesarzxk/nuuvia-vpn/security/advisories/new

Include:

- affected version and platform;
- steps to reproduce;
- impact and any proof of concept;
- whether the issue affects the CLI, GUI, relay server, or bundled installer.

You will receive an acknowledgement as soon as possible. Please allow time for a
fix and coordinated disclosure before publishing details.

## Scope

In scope:

- the Rust workspace in this repository;
- Windows/Linux GUI and CLI;
- rendezvous/relay server code in `crates/nuvia-server`;
- packaging scripts, installers, and GitHub Actions workflows.

Out of scope:

- vulnerabilities in third-party dependencies that are already fixed upstream;
- the hosted relay infrastructure itself (report privately and we will route it);
- social engineering or physical attacks.

## Practices

- MFA is required for GitHub and SignPath access.
- Release artifacts are built by GitHub Actions from this repository.
- Windows signing requests are reviewed and approved manually.
- Dependencies are checked in CI with `cargo clippy`, tests, and the workspace
  lockfile.
