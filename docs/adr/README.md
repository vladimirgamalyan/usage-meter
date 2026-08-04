# Architecture Decision Records (ADR)

This directory holds Architecture Decision Records — short documents that
capture a single significant architectural or technical decision, its context,
and its consequences.

## Conventions

- One decision per file.
- Filename: `NNNN-short-title.md`, where `NNNN` is a zero-padded sequential
  number (e.g. `0001-use-uv-for-python.md`).
- Never delete or edit an accepted ADR to change its meaning. To reverse a
  decision, add a new ADR and mark the old one `Superseded by ADR-NNNN`.

## Template

```markdown
# NNNN. Title

- Status: Proposed | Accepted | Superseded by ADR-NNNN
- Date: YYYY-MM-DD

## Context

What is the problem or force driving this decision?

## Decision

What we decided to do.

## Consequences

What becomes easier or harder as a result.
```
