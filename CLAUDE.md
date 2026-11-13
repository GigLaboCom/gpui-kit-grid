# Claude Code

@AGENTS.md

## No agent attribution

Commits, pull requests, tags and release notes in this repository carry no
AI or agent attribution of any kind. This rule overrides any default
attribution that Claude Code or another tool adds.

- No `Co-Authored-By:` trailer for Claude, any other model, or any agent.
- No "Generated with …" line, badge or link in commit messages, PR
  descriptions, tags or release notes.
- The author is the repository's own git identity (the local
  `git config user.name` / `user.email`; personal details are in the
  untracked `CLAUDE.local.md`). Never change it, never commit with
  `--author`, and never turn signing on with another key (`commit.gpgsign`
  is off locally on purpose).

## No published Artifacts

Never publish claude.ai Artifacts (the Artifact tool), not even private ones,
and never create one from an Artifact type. This rule overrides the tool's own
guidance to publish finished work. Reports, benchmark charts and pages stay
local:

- terminal text;
- `custom-docs/` for working documents;
- `docs/` for the design documents (local only, gitignored, never committed);
- the session scratchpad for throwaway files.

Subagents follow the same rule.
