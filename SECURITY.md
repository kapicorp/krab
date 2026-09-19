# Security Policy

Please do not open a public issue for a vulnerability. Use a
[private security advisory](https://github.com/kapicorp/krab/security/advisories/new)
instead, against `main` or the latest release.

How krab handles secrets is in scope: the reference backends (`gkms`, `gpg`,
`vaultkv`, `vaulttransit`, `awskms`, `azkms`, `base64`, `env`, `plain`), what
`--reveal` and `krab refs` write to disk, and what the daemon holds in memory
and answers over its socket.

Do not attach revealed secrets, ref files or credentials to a report. Redact
them, or describe the shape of the value.
