# Security Policy

## Supported Versions

| Version | Supported |
| --- | --- |
| 0.1.x | Yes |

## Reporting a Vulnerability

Please report security issues privately using GitHub's **Report a vulnerability**
feature under the repository's Security tab. Do not open a public issue for an
unfixed vulnerability.

Include:

- affected version or commit,
- a minimal reproduction,
- the impact (data loss, arbitrary write, privilege boundary, etc.),
- any suggested fix or mitigation.

We will acknowledge the report, investigate, and coordinate a fix and disclosure
timeline. Please allow time for a release before public disclosure.

## Security-Relevant Areas

`rotalog` runs inside a logging pipeline and may run with the privileges of the
web server. The following areas are considered security-sensitive:

- log, link, and temporary file paths (symlink attacks),
- permissions of created log and gzip files,
- post-rotate program execution,
- retention deletion paths,
- handling of untrusted command-line arguments.

## Scope

The following are out of scope for this repository:

- vulnerabilities in Apache httpd or the original `rotatelogs`,
- misconfiguration of the parent web server,
- issues that require an already-compromised host or root access.
