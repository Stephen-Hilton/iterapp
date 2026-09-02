# Requirement: readable questions — a persistent Answer lightbox

Filed by Stephen (via the pdy-dev session), 2026-08-28. Diagnosed against the current
source tree at `src/webapp/app.html` and `src/server.rs`; line numbers are from that
tree as of 2026-08-28 and may drift — the function names are the durable anchors.

## The problem being solved

A work item's question can be several thousand characters. Reading it in the webapp is
currently impossible when the engine is busy: every few seconds the reader's scroll
position jumps back to the top, both inside the question's own scroll box and at the
page level. "Pause & Edit" as an escape route fails too (see Defect 2).

## The measured mechanism (diagnosed 2026-08-28, running instance on port 9717)

**Defect 1 — any database write anywhere resets the reader's scroll.**
The server's SSE loop (`sse_events()`, `src/server.rs:2204`, sleep at :2223) stats
`iter.db` and `iter.db-wal` every 700 ms and emits an event on any size/mtime change —
so every agent heartbeat, log row, or attempt-counter write fires one. When no visible
row changed, the client still receives an empty delta (`{"type":"delta","changed":[],
...}`). `applyDelta()` (`app.html:1330`) does not check for emptiness: it ends with an
unconditional `render()`, and `renderRows()` (`app.html:1697`) rebuilds the whole list
with `$('rows').innerHTML = ...`. The question is rendered into a `.textblock` scroll
container (`max-height: 60vh; overflow: auto`, `app.html:239`), so the rebuild destroys
the scrolled element and the new one starts at `scrollTop = 0`. Separately, when a delta
carries a summary row, `applyDelta`'s `put()` calls `bodyLoaded.delete(workid)`; the next
render paints the open row as a one-line "Loading…" placeholder, the document shortens,
and the browser clamps `window.scrollY` — the page-level jump.

**Defect 2 — Pause & Edit races its own side effect and refuses to open.**
The `pauseedit` handler (`app.html:2747`) POSTs `pause`, which writes the database, which
fires an SSE event, whose debounced handler runs a second `loadAll()` concurrently with
the first chain. `loadAll()` replaces the `items` array outright (`app.html:1276`), so
the final `items.find(...)` can return a fresh summary object, and `openForm`'s
summary guard (`app.html:2226`) bails with the toast "Still loading this item's request —
try again in a moment". On screen this reads as a modal that flashed and vanished.
(Verified: no code path from the refresh machinery reaches `closeLb()` — the overlay is a
sibling of `.app` and already survives rebuilds. The modal never opened.)

## Required behavior

**R1 (required): repaints hold while a lightbox is open.** Generalize the existing
focus-freeze in `render()` (`app.html:2044`: `if (document.querySelector(
'.answerbox:focus')) { renderHeld = true; return; }`) to also hold when `#overlay` has
class `open`. `closeLb()` (`app.html:2066`) replays a held render on close. `syncBodies()`
keeps running ahead of the check, as it does today — only the paint defers. Rationale: an
open lightbox is the reader's full attention; the queue behind it can wait. This also
defuses Defect 2 in practice.

**R2 (required): an "Answer…" entry in the Actions context menu.** For any item that has
a question and is in an answerable state (`ANSWERABLE`, `app.html:1807`), the Actions menu
(`actionButtons()`, `app.html:1960`; `question` branch at :2003) gains an **Answer…**
entry, first in the list. It opens a lightbox (via the existing `openLb()`, into
`#overlay`) containing:

- the item's full question in a scroll box (its own `max-height`, e.g. 44vh, so the
  answer area stays visible),
- an answer `textarea` wired into the existing draft machinery (`data-answer` attribute,
  `answerDrafts`, `flushAnswer`, `answerStatus`) so autosave behaves identically to the
  in-row box,
- **Close** and **Answer and Queue** buttons; the latter flushes the draft, calls the
  existing `answerAndQueue()`, and closes. For non-answerable states that still carry a
  question, the lightbox opens read-only (question plus recorded answer, Close only).

The workitem list may keep updating behind the modal once R1 lands — that is fine and
expected; the modal owns its own copy of the question text and its own scroll position.

Known hazard to handle: with the row also expanded, two elements share the same
`data-answer` / `data-astat` id and the row's copy comes first in document order.
`setAnswerState()` (`app.html:1864`) must update all matches (`querySelectorAll`), and
`answerAndQueue()` (`app.html:1915`) must read from the overlay's box when the overlay is
open, not the document's first match. Follow the construction pattern of `confirmStop()`
(`app.html:2557`) / `releaseGated()` (`app.html:2663`): build with `openLb()`, wire the
footer button with `addEventListener` immediately after. If the item is still a summary,
`await ensureBody(id)` before building, and toast-and-return if it stays a summary.

**R3 (recommended, root cause): the server stops emitting empty deltas.** In
`sse_events()` (`src/server.rs`, delta assembly around :2181–2190): when `changed`,
`removed`, and `archived` are all empty AND `counts` is unchanged from the previous send,
skip the write entirely. The WAL moves far more often than a visible row does, so this
removes most refresh traffic at the source. R1/R2 must not depend on R3 being done.

**R4 (optional, only if in-row reading should also survive):** in `renderRows()`,
capture each scrolled `.textblock`'s `scrollTop` (keyed by row id + index) and
`window.scrollY` before the `innerHTML` assignment, restore both after. This is a patch
over the churn, not a stop to it — lowest priority.

## Non-goals

- No refresh-rate configuration knob is being requested.
- The list behind the modal is allowed to keep moving; do not freeze data flow, only paint.
- No change to the answer/queue API contract.

## Acceptance criteria (each per the break-it discipline: show the red before the green)

1. With the engine live and agents writing (or a loop touching `iter.db-wal`), open
   Answer… on an item with a multi-screen question, scroll to the bottom, and wait
   through at least ten SSE events: the modal stays open and the scroll position does
   not move. Control: demonstrate that before the change, the in-row question box does
   lose its scroll under the same write load.
2. Type a partial answer in the modal, close without submitting, reopen: the draft is
   there (same autosave behavior as the in-row box). Type in the modal while the row is
   also expanded: the row's status line and the modal's status line both update, and
   Answer and Queue submits the modal's text, not a stale row copy.
3. Answer and Queue from the modal transitions the item exactly as the in-row Answer
   and Queue does today (state change, draft cleared, toast).
4. Close the modal after updates arrived while it was open: the held repaint replays
   immediately (list reflects current state without waiting for the next event).
5. Pause & Edit on a question item under live write load opens reliably (no
   "Still loading…" bail) or, at minimum, is demonstrated fixed as a consequence of R1
   with a recorded before/after.
6. If R3 is done: instrument or log the SSE stream and show that WAL-only churn produces
   no client events, while a real row change still arrives within ~1 s.

## Deployment note (for the pdy-dev side, not this repo)

The instance the pdy-dev programme uses (port 9717) runs the checked-in binary copy at
`pdy-dev/devops/iter`, which is currently two days older than this source tree (verified:
the served page differs from `app.html` only in the Settings-form region). After this
lands, pdy-dev needs a rebuilt binary copied over; Stephen coordinates that step.
