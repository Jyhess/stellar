@AGENTS.md

## Claude Code

- Use `/core:clarify` before designing anything non-trivial.
- Claude Code plugins for this project are listed in `.claude/settings.json` and take effect at the next session, or after `/reload-plugins`.
- Project skills (`.claude/skills/`):
  - `code-map`: using and maintaining `docs/code-map.md`;
  - `new-feature`: checklist for any user-visible change (questions, translations, docs, tests);
  - `file-format`: mandatory procedure before touching a saved or recorded structure;
  - `perf-measurement`: benchmarks, profiling, reference timings, known performance leads.
