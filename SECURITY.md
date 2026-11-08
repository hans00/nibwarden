# Security policy

nibwarden handles password vaults, so security problems are taken seriously.

## Reporting a vulnerability

Please do not open a public issue. Report it privately instead:

- through GitHub's [private vulnerability reporting](https://github.com/hans00/nibwarden/security/advisories/new), or
- by email to me@hans00.me.

Include what you found, how to reproduce it, and which version or commit you tested. Once
a fix is released, the advisory is published with credit to you, unless you prefer
otherwise.

Problems in the Bitwarden SDK or servers themselves belong to
[Bitwarden's own security program](https://hackerone.com/bitwarden).

## Supported versions

Only the latest release receives security fixes.

## Scope

In scope, for example: secrets leaking to disk, logs or other users; a program getting
secrets or signatures without the approval the settings require; the approval or unlock
windows being answered or spoofed by another program; the browser extension channel.

The limits described in the README are known: a program running as you that can trace or
inject into other processes, or read the keyboard on X11, is outside what nibwarden can
defend against.
