# Contributing to nibwarden

Thank you for helping. Bug reports, translations and pull requests are welcome.

## Reporting bugs

Open an [issue](https://github.com/hans00/nibwarden/issues) with your distribution and
desktop, the server (bitwarden.com, bitwarden.eu or Vaultwarden and its version) and what
happened. `journalctl --user -u nibwardend` has the daemon's log; it never contains
secrets. Security problems go through [SECURITY.md](SECURITY.md) instead.

## Building and testing

See the README for the build dependencies. Before sending a change:

```sh
cargo xtask check    # formatting, clippy (no warnings) and the unit tests
scripts/e2e.sh       # needs docker, jq, python3, git, OpenSSH and util-linux
```

GUI changes: render the windows with
`cargo run -p nibwarden-gui --example snapshot -- <dir>` and look at them in light and
dark, in English and Chinese.

## Ground rules of the code

- **Only `nibwarden-core` uses the Bitwarden SDK.** The SDK is unstable; every other crate
  sees nibwarden's own types.
- **Everything OS-specific lives in `nibwarden-platform`.** The other crates contain no
  `cfg(target_os)`. Other systems compile against stubs; CI checks that they still do
  (locally: `cargo xtask check --target <triple>`).
- **Only the daemon holds keys.** The GUI and the CLI never see the vault's keys; secrets
  pass through them only when the user asked for one, held in `Zeroizing` types.
- **Ask before use.** Any new way for a program to get a secret goes through the approval
  rules in `crates/nibwarden-daemon/src/approvals.rs`.
- **Text shown to users** is translated: Slint strings with `@tr(...)`, translations in
  `crates/nibwarden-gui/lang/*/LC_MESSAGES/nibwarden-gui.po`. The daemon sends codes,
  not sentences, so the windows can word them.
- Match the surrounding code: its comment style, naming and error handling. Comments say
  why, not what.

The design documents in `docs/architecture` (in Chinese) explain the decisions behind
these rules.

## Translations

Add or fix entries in the `.po` files and check them with `msgfmt -c`. A new language
needs a directory under `crates/nibwarden-gui/lang/` and an entry in the language
setting.

## Pull requests

Keep a pull request to one change, explain why it is needed, and add a test where one
can catch the problem: a unit test, or a check in `scripts/e2e.sh` for anything that
goes through the daemon. By contributing you agree that your work is licensed under the
GPL-3.0-only, like the rest of nibwarden.
