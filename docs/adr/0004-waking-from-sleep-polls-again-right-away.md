# 0004. Waking from sleep polls again right away

- Status: Accepted
- Date: 2026-08-07

## Context

A laptop that slept for hours comes back with numbers as old as the sleep. Two
things kept them that way longer than necessary.

The poll timer itself is not the problem: `SetTimer` counts biased interrupt
time, which includes time spent asleep, so a timer whose interval elapsed during
the sleep fires as soon as the message loop runs again. That first poll,
however, lands in the seconds right after a resume, when the network stack is
usually not back yet. It fails, and the transient-failure backoff then waits 30
seconds before the next attempt — the delay actually observed after a resume.

The 429 cooldown on the Claude usage endpoint had the opposite flaw. It was an
`Instant` deadline, and `Instant` (a performance counter) does not advance while
the machine sleeps. A cooldown started before a night's sleep still had its full
remaining time to run in the morning, holding the endpoint back long after the
rate limit had reset.

## Decision

Handle `WM_POWERBROADCAST` for `PBT_APMRESUMEAUTOMATIC` and
`PBT_APMRESUMESUSPEND`: poll immediately, reset the retry count, and grant a
window of fast poll rounds (the existing 5-second timer, 12 attempts, about a
minute). The window ends on the first successful round; when it runs out, the
regular backoff takes over. Polling paused on an auth error is left alone — it
watches the credential stores on the poll timer and has nothing to fetch.

Keep the 429 cooldown on `GetTickCount64`, which counts time spent asleep, so a
cooldown that outlived the sleep is over on wake-up. It is not cleared on
resume: a short nap must not become a way to poll a rate-limited endpoint again.

## Consequences

- After a resume, values refresh within a few seconds of the network coming
  back rather than after a fixed half minute.
- A resume with no network at all costs up to 12 connection attempts spread over
  a minute, then falls silent on the normal backoff.
- The wake-up handler cannot tell a user-initiated resume from a maintenance
  wake-up, so a machine that wakes on a timer polls too. The cost is one round.
- `Instant` is still used for child-process timeouts, where a sleep in the
  middle would be pathological anyway.
