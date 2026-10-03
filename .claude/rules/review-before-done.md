# Code changes require review before they are done

No code change is complete until it has been reviewed by **both** the `code-reviewer` and `security-analyst` subagents, and every finding they report has been addressed.

- After implementing a change, launch `code-reviewer` and `security-analyst` in parallel (a single message with both Agent calls), each scoped to the changed files and the diff.
- "Addressed" means one of:
  - Fixed. Re-run the reviewer that raised it to confirm.
  - Explicitly rebutted with evidence (code reference, test, or reproduction) showing the finding is wrong.
  - Deferred by the user. Only the user can approve deferring a finding; record what was deferred.
- After fixing findings, re-run the reviews on the new diff until both come back with no unaddressed findings.
- Do not report a task as done, commit, or open a PR while findings are outstanding. If you stop early, say plainly which findings remain open.
- Only edits to docs or comments that leave code and config behaviour unchanged are exempt.
