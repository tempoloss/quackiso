# 5. `threads := n` is a request, capped at four times the machine

Status: accepted

Decided in `003348a` (2026-08-07), alongside the path-resolution fixes in the
same table-function layer.

## Context

A glob is scanned one worker per file. The default is
`available_parallelism().min(nfiles)`, and `threads := n` overrides it.

The override was capped only at the file count. Over a large glob,
`threads := 100000` spawned a hundred thousand OS threads: on Linux that is
about 800 MB of stack reservation before a single byte of XML is read, and on
Windows it is a thread-creation failure partway through with half a scan
running. Neither is a plausible thing for a user to want, and neither produces a
diagnostic that says what happened.

The work each thread does is parsing one file front to back. It is CPU-bound with
a small, bounded working set and no blocking I/O worth overlapping, so past the
machine's parallelism an extra thread costs a stack and a context switch and buys
nothing measurable. The recorded speedup is 6.9x on 8 files of 35 MB.

## Decision

```
Some(n) if n >= 1 => (n as usize).min(nfiles).min(auto * 4),
```

An explicit request still wins over the default, and still cannot exceed the file
count, and is now also capped at four times `available_parallelism()`.

Four, not one. Oversubscription is sometimes the right answer -- files of wildly
uneven size finish unevenly, and a few extra workers keep the tail busy -- so the
cap is set where it stops being a tuning knob and starts being a mistake, not
where the machine is nominally saturated.

The request is not rejected and no warning is emitted. `threads := 100000` is a
number someone typed to mean "as many as possible", and answering it with the
most that helps is the useful reading.

## Alternatives rejected

**No cap.** The status quo. It treats a typo as a specification.

**Error on an unreasonable value.** There is no non-arbitrary threshold to error
at, and failing a query over a scheduling hint is a worse outcome than running it
slightly differently from what was asked.

**Cap at `auto` exactly.** Removes the ability to oversubscribe deliberately,
which is a real technique on a corpus with uneven file sizes.

## Consequences

`auto * 4` is a chosen ceiling, not a measured one. If a real workload wants more,
raise the multiplier; do not remove the cap, because the failure it prevents is
resource exhaustion during a scan rather than a slow scan.

The published contract changed: `README.md` and
`community-extension/description.yml` said `threads := n` pins the pool, and both
now say it is capped. `effective_threads` in `src/lib.rs` is the only place the
rule lives.
