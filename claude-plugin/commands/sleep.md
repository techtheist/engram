---
description: Sleep — consolidate this project's Engram graph: work its subjects (piles of close notes), write a state-of note per settled subject, merge restatements, link what replaced what, mark the rest reviewed.
---

The user wants an Engram sleep sweep.

Invoke the `engram-sleep` skill and follow it end to end: settle conflicts first (`list_nodes` `view: "conflicts"`), then delegate each subject to a fresh subagent with the skill's brief, read subjects with `list_nodes` `view: "review"` (or `around` for a named subject), tie islands back in last (`view: "islands"`), write the state-of note for settled history, merge restatements, link supersessions and answers, judge listed suspects, mark every note left standing `reviewed`, and finish with the audible report, including how to undo the session in the pane.

If the `engram` MCP tools are not available in this session, stop and run `/engram:setup` first.

$ARGUMENTS names a subject (a tag, an anchor or note id, or a phrase to search) when present; otherwise work the review queue from the top, one subject per subagent, up to three subjects.
