## General guidelines

Before getting started, read `docs/issues.ignore.md` for open questions. After a problem has been solved:

1. If that answer is relevant to later milestones, migrate it to `decisions.ignore.md`.
2. If that answer is temporary and only relevant to the current milestone, remove it once you are done implementing it.

Keep codebase clean and well-structured, and abstractions clean. No boilerplate code or redundant files, but never grow a file too long if this can be avoided.

For code agents: When you are implementing a feature, do not write persistent tests for it. Passing compilation is sufficient. Testing agents will handle the test creation, so if you are told to "pass XXX test", do so.

If tests contain issues, be alert! Do not always assume that your code is wrong. If you think the test is either ambiguous or touches an area that has not been fully specified, raise the issue to the user in `docs/issues.ignore.md` and describe the status in `docs/status.ignore.md` (what remains to be done, what blocks you).

NEVER implement a feature without full understanding of the requirements. If you are not sure about something, ask for clarification in `docs/issues.ignore.md` and wait for the answer before proceeding. Make no assumptions on your own.

When implementing the codebase, you may use any external library you wish.
