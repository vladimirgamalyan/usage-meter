# 0003. A rate-limited usage endpoint is waited out, not worked around

- Status: Accepted
- Date: 2026-08-06

## Context

Claude Code usage is read from `/api/oauth/usage`, a JSON endpoint that is the
only source of model-scoped weekly limits (the separate Fable window). Any
non-success answer fell back to the Messages API, whose unified rate-limit
headers carry the 5-hour and 7-day windows but no scoped limits at all.

Polling every minute — the shortest interval the menu offered — drove that
endpoint into answering 429. The fallback then hid the failure twice over: the
scoped row silently disappeared while the shared windows kept updating, and the
poll counted as successful, so the next round came on schedule and kept the
limit engaged. Worse, the fallback is a real `POST /v1/messages` call, so a
monitor that exists to watch the quota was spending it once per poll.

## Decision

Treat 429 from the usage endpoint as a distinct `PollError::RateLimited`:

- No Messages API fallback for that status. Other failures (transport errors,
  5xx, unparseable JSON) still fall back as before.
- The endpoint enters a cooldown — 10 minutes, doubling per consecutive 429,
  capped at an hour — during which it is not contacted at all. A successful
  answer clears it, and so does an explicit **Refresh**.
- The retry backoff is skipped for this error: its delays are capped at the
  poll interval, so retrying early would only hammer a limit that is already
  engaged. The regular interval applies instead.
- The shortest selectable update interval is now 5 minutes; a settings file
  naming the dropped 1 minute falls back to the default.

A provider that did not answer keeps the values from the previous round rather
than dropping to an error marker, and its whole block — bars, labels, values,
elapsed pointer — is drawn blended toward the background.

## Consequences

- The scoped row no longer vanishes when the endpoint is unavailable: it stays
  on screen, faded, and the window does not resize under the pointer.
- Countdowns keep ticking on stale rows, so their reset times stay truthful
  while the percentages age.
- A cold start against a rate-limited endpoint shows nothing for Claude Code
  (there is no previous round to fall back on) until the cooldown expires. The
  alternative — one fallback call for the shared windows — was rejected: it
  spends quota and cannot restore the row that matters.
- Staleness is per provider, not per row: Claude Code, Codex and Antigravity
  fade independently, but a partial failure inside one provider cannot be
  expressed.
