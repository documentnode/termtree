# Security Policy

## Reporting a vulnerability

**Please do not report security vulnerabilities through public GitHub issues.**

Email **[support@documentnode.io](mailto:support@documentnode.io)** with `SECURITY` in the subject
line. Include as much of the following as you can:

- What the issue is and why you believe it is a security problem
- The TermTree version and your operating system
- Steps to reproduce, or a proof of concept
- What an attacker could achieve with it

You will get an acknowledgement that a human has read your report. We will keep you informed as we
investigate, and we will tell you when a fix ships. If you would like credit in the release notes,
say so and tell us how you would like to be named.

Please give us a reasonable opportunity to fix the issue before disclosing it publicly.

## Supported versions

TermTree is in beta and ships as a single release line. Only the latest released version receives
security fixes. Update from within the app, or download the current version at
[termtree.com/download](https://termtree.com/download).

## Scope

In scope: the TermTree desktop application, its update mechanism, its optional cloud sync, and the
install script served at <https://termtree.com/install.sh>.

Out of scope: the `benchmark/` harness published in this repository. It is a local developer tool
you build and run yourself — it launches other applications on your own machine under your own
account, and writes only to its own disposable scratch directory. Report bugs in it as an issue,
not as a security report.

Out of scope: the security of programs you choose to run inside TermTree's terminals. Those are
ordinary terminal sessions — Claude Code, Codex, shells, servers, and other CLIs run with your own
credentials under their own security models, exactly as they would in any other terminal emulator.
