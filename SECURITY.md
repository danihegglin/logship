# Security Policy

## Supported versions

Every push to `main` is released as a new patch version, so fixes ship in the
next release rather than being backported. Only the latest release on the
[Releases page](https://github.com/danihegglin/logship/releases) receives
security fixes. If you're on an older version, update before reporting.

## Reporting a vulnerability

Please **don't open a public issue** for security problems.

Report them privately through GitHub instead: open the repository's
**Security** tab and click **Report a vulnerability**
([direct link](https://github.com/danihegglin/logship/security/advisories/new)).
Only the maintainers can see the report.

Include as much of this as you can:

- the logship version (the release tag or the file you downloaded) and your
  OS, desktop and architecture
- what an attacker can do and what they need (for example "a crafted log file
  opened in logship")
- steps or a sample file that reproduce it; a small one is best
- any error or crash output; start logship from a terminal to see it

### What to expect

- An acknowledgement within a week.
- An assessment, and a fix or a plan, within 30 days for confirmed issues.
- A GitHub security advisory and a patched release once the fix is out,
  crediting you unless you'd rather stay anonymous.

This is a small project maintained in spare time, so please allow for some
delay, and give us a reasonable chance to release a fix before disclosing.

## Scope

logship is a local desktop app: it opens files you choose, memory-maps them
read-only, and makes no network connections. The main thing it has to defend
against is **untrusted log files**. In scope, for example:

- a crafted file that runs code, reads or writes other files, or crashes
  logship in a way an attacker could exploit
- content that makes logship display misleading output, such as escape
  sequences that survive the ANSI stripping and change what other lines show
- searches or filters that hang or exhaust memory on a reasonably sized file
- problems in the release packages or the workflows that build them, such as
  a tampered or mislabeled download

Usually out of scope:

- running out of memory or disk on very large files: the line index costs
  about 9 bytes per line, so a 100M-line file needs roughly 900 MB
- crashes caused by another process truncating or rewriting the file while
  logship has it mapped, unless it leads to something more than the crash
- macOS builds that aren't notarized (only releases signed with the
  maintainer's Developer ID are) and the unsigned Windows build, which are
  known (see "Verifying downloads" below)
- vulnerabilities in dependencies with no impact on logship; please report
  those to the dependency's maintainers

## Verifying downloads

Each release includes `SHA256SUMS.txt` with the checksum of every file. Before
installing a download, compute its checksum and compare it with the matching
line in that file:

```sh
sha256sum logship-*.rpm             # Linux
shasum -a 256 logship-*.dmg         # macOS
Get-FileHash logship-*.zip          # Windows (PowerShell)
```

Releases are built only by the GitHub Actions workflows in
`.github/workflows/`, from the tagged commit. The Windows build is not
code-signed. The macOS app is notarized when the release was signed with the
maintainer's Developer ID; `spctl --assess --verbose /Applications/logship.app`
then says `accepted` and names the developer. Otherwise it is only ad-hoc
signed.
