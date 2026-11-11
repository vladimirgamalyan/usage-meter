# 0005. Claude usage is requested as the Claude Code CLI

- Status: Accepted
- Date: 2026-11-11

## Context

Claude Code accounts can be granted limit resets: a weekly or full reset, or a
Fable reset that gives back what Fable used of the weekly limit. The usage
endpoint lists them in a `cedar_ember` block when asked with `cedar_ember=1`,
but only to the Claude Code CLI. Any other client gets the block with
`"eligible": false, "ineligible_reason": "surface"` and no grants.

The server tells the CLI apart by its User-Agent,
`claude-cli/<version> (external, cli)`. Checked on 2026-11-11 against a real
account: `x-app: cli` alone still answers `surface`; adding the CLI's
User-Agent makes the account eligible and lists its grants.

## Decision

The usage request sends the CLI's User-Agent and `x-app: cli`, with the version
the check was made against written into the code. Nothing else changes: the
Messages API fallback and the other providers keep their own requests, and the
resets are only read, never claimed.

## Consequences

- The resets of the signed-in account are shown under its Claude Code rows.
  The request already carried the CLI's OAuth token; it now also carries the
  CLI's name, so the server counts it as a CLI request.
- If the server stops accepting the string (a minimum version, a different
  check), the list goes empty without an error and the rows disappear.
  `--diagnose` logs the reason the account is not eligible, which tells this
  apart from simply having no resets left; the fix is a newer version string.
