# Prefer subagents

Delegate to subagents wherever it helps. It keeps the main context focused on decisions, and lets independent work run in parallel.

- **Match the agent to the task:**
  - `rust-developer` for implementation, bug fixes and refactors.
  - `researcher` for external questions such as package-manager flags, package names across backends, and how comparable tools behave.
  - `code-reviewer` and `security-analyst` for review (see `review-before-done.md`).
  - `Explore` for broad codebase searches where you only need the conclusion.
- **Parallelize:** when tasks are independent, launch all of their agents in a single message rather than one at a time.
- **Chunk sensibly:** for wide work like repo-wide reviews or multi-module changes, split by subsystem (about a dozen chunks, with very large files on their own), not one agent per file.
- **Write self-contained prompts:** state the files, the goal, what to verify, and the expected output format. Subagents don't see this conversation.
- **Stay in the loop:** read and verify subagent results before acting on or reporting them. Relay the conclusions, not the raw dumps.
- **Do it yourself when it's small:** a single-file edit, a lookup in a known file, or a quick command.
