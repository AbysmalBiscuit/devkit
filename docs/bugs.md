## `issue prs` not showing correct status after changes made

**Status:** RESOLVED 2026-06-24. The `await re-review` action now fires only when
a reviewer who requested changes is back in `reviewRequests` (GitHub drops a
reviewer from that list once they review, so reappearance = re-review requested),
replacing the old "my latest review is newer than theirs" heuristic — which tripped
on `COMMENTED` reviews created by replying to an inline thread (e.g. a bot's). The
review/status column also now reports `changes` whenever any actor (human or bot,
required or not) has a standing change request, not just when GitHub's
`reviewDecision` surfaces it. The original report is kept for context.

> When updating an open pr, it detects it as `address changes → replied; await re-review`, but it should.
> It should only show that when re-review has been requested.
> For that PR all I did was push 2 commits and reply to greptile, not  to the human reviewer. I also didn't re-request review from the human reviewer.
