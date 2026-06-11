## General guidelines

The `docs/` folder contains several documentation for agent work. `status.ignore.md` tracks the current working status: What has been done and what not. `questions.ignore.md` tracks open questions that need to be resolved before implementation. `decisions.ignore.md` tracks design decisions that have been made. `todo.ignore.md` tracks future work that needs be done on user demand.

### Review Agents

If you are told to review some feature or identify root causes of issues, your results should be prepended to `docs/status.ignore.md` and should include what is finished, what is unfinished, what is implemented wrongly.

### Plan Agents

If you are told to plan a feature or a fix, read `docs/questions.ignore.md` for related open questions first, and raise questions if you are unsure. Refer to `docs/status.ignore.md` for analyzed status and `docs/decisions.ignore.md` for design decisions.

The plan should be in the form of a list of tasks, which you can organize and place inside `docs/todo.ignore.md`. Each task should be clear and actionable. If this task clears an issue in `docs/status.ignore.md`, mention it, so that after the code agent finishes the task, the stale status can be substituted.

### Code Agents

If you are told to implement a feature, read `docs/questions.ignore.md` for related open questions first, and raise questions if you are unsure. When you are implementing a feature, do not write persistent tests for it. Passing compilation is sufficient. Testing agents will handle the test creation, so if you are told to "pass XXX test", do so.

NEVER implement a feature without full understanding of the requirements. If you are not sure about something, ask for clarification in `docs/questions.ignore.md` and wait for the answer before proceeding. Make no assumptions on your own.

After a problem has been solved:

1. If that answer is relevant to later milestones, migrate it to `decisions.ignore.md`.
2. If that answer is temporary and only relevant to the current task, remove it once you are done implementing it.
3. If that problem is in the todo list, tick it in the list. If it is linked to `docs/status.ignore.md`, update the status as well.

Keep codebase clean and well-structured, and abstractions clean. No boilerplate code or redundant files, but never grow a file too long if this can be avoided.

If tests contain issues, be alert! Do not always assume that your code is wrong. If you think the test is either ambiguous or touches an area that has not been fully specified, raise the issue to the user in `docs/questions.ignore.md` and describe the status in `docs/status.ignore.md` (what remains to be done, what blocks you).

When implementing the codebase, you may use any external library you wish.

When writing documents, keep your writing clear, concise, to-the-point and human-readable. Avoid technical jargon, non-standard terminology like "facade", and unnecessary details. Never repeat yourself.

Do not overwrite or delete existing, unrelated content in any documentation, tracked by git or not. If you need to update something, prepend or append to the existing content, or create a new section if necessary. Only delete content if it has been resolved, is already false, or has met criteria for removal as specified above. In any of these cases, you should report this removal when you finish working.

### Profile Agents

If you are told to profile and improve the performance of Vox, you should strive to make Vox as fast as possible. The expected running time of Vox when SOpt is enabled is close to native binary compiled by Cpp, and IOpt speed should be close to or better than Python.

When profiling, always rely on real data and metrics. Never make assumptions based on code alone: Run them to obtain concrete results. If investigating SOpt, obtain real wasm code and ensure that the code is accepted by the compiler (silent fallback is obselete: if it happens flag it as a flag in `todo.status.md` and resolve it later on). If investigating IOpt, obtain the MIR code generated.

You can always use `perf` to profile code. You can also add time checkpoints or debug logs in the code so long as you remove them later on. If you add such, ensure to document them in `docs/status.ignore.md` **immediately** after you add them, and remove these entries once you remove the checkpoints or logs.
