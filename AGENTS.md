# Stellar — shared project instructions

3D gravitational N-body simulator in Rust (wgpu + egui) for Windows. Development machine: Windows, NVIDIA RTX 4060 Ti (8 GB dedicated), 16 GB of RAM.

Read before any substantial change: `SPEC.md` (rules in force), `README.md` (usage), and the page of `docs/` for the area (list in the README; `history.md` for the history of decisions).

**To find where to change something, start with `docs/code-map.md`** (files, key symbols, cross-layer change paths), then navigate by symbol with rust-analyzer. Keep the map up to date with the `code-map` skill.

## Communication and languages

- Talk to Julien in **elaborate French with a rich vocabulary**; do not try to prolong the discussion.
- Documentation and skills are in English; code, comments (WGSL included), CLI option names, commit messages and developer-facing text (panics, tests, examples, `STELLAR_STATS`) are in English.
- The interface has English source strings translated into French, error messages included: rules in the `new-feature` skill.

## Shared rules

- Shared rules: `~/.dev-kit/rules/shared-rules.md`; Claude Code gets them from the core plugin, other agents read the file at session start; a section of this file that contradicts them wins.

## Commands

```powershell
cargo build --release                      # target\release\stellar.exe and stellar-cli.exe
cargo test --workspace                     # GPU tests are skipped without a card
cargo clippy --workspace --all-targets
```

Quick starts (`STELLAR_AUTOSTART`, `STELLAR_LOAD`, `STELLAR_ENGINE`…), measurement tools and software rendering (`WGPU_BACKEND=gl`): `docs/testing.md`.

## Documentation and tracking

- Documentation in `docs/`; tasks in the GitHub issues of this repository.
- Time spent is recorded in the work journal (plugin `dev-journal`).

| Change | Files to update |
|---|---|
| Any user-visible feature | `README.md` (usage), `SPEC.md` (rules in force), a line in `docs/history.md` |
| Source file, module or responsibility added, moved or renamed | `docs/code-map.md` (`code-map` skill) |
| Crate, dependency between crates, layer rule | `docs/architecture.puml` (`architecture.py --write`), `docs/architecture.md` |
| Serialized structure | `docs/formats.md` (`file-format` skill) |
| GPU kernel, step sequence or constant | `docs/gpu-engine.md` |
| CPU engine step | `docs/cpu-engine.md` |
| Sub-simulation rule | `docs/subsystems.md` |
| Environment variable, test area, measurement tool | `docs/testing.md` |

## Design

- Before designing or changing code, read `docs/design/principles.md` (errors, robustness by design).

## Architecture

- Layer rules, crates and adding an engine: `docs/architecture.md`. Components and allowed dependencies: `docs/architecture.puml`, checked by `python3 .github/scripts/architecture.py` with the verification.
- Changing these components or their checks is an architecture change, approved by Julien beforehand.

## Delicate points

Read the page before touching its area; the traps that cost the most:

- **GPU engine** (`docs/gpu-engine.md`): data taken from `snapshot.state` stays consistent with `state.step`, not `snapshot.step` (the full state lags); accessors never read the device back; keep Mailbox presentation (`present_mode`) and Vulkan (DirectX 12 compiles shaders for ~100 s).
- **Sub-simulations** (`docs/subsystems.md`): the engine is always wrapped in `MultiSystem`; `state()` is in global coordinates.
- **Rendering and camera** (`docs/architecture.md`): positions relative to the anchor; instance buffers in chunks.
- **File formats**: any field change of a saved or recorded structure needs a new version: use the `file-format` skill.
- **User files**: repositories delete to the recycle bin (`with_trash`), tests delete permanently in a temporary folder. Files in `saves\` and `recordings\` can weigh tens of gigabytes: never modify them for a test.

## Verification

1. **Before running tests**: no `stellar` / `stellar-cli` process may be running (it shares the GPU and makes the GPU tests hang); if one is, ask Julien to close it.
2. **Before each commit**: `cargo test --workspace --exclude stellar-engine-gpu` and `cargo clippy --workspace --all-targets -- -D warnings`.
3. **Before handing over**: the GPU tests one at a time (`cargo test -p stellar-engine-gpu -- --test-threads=1`; in parallel they hang), then `cargo build --release` and copy `stellar.exe` and `stellar-cli.exe` from `target\release\` to the repository root. Julien checks the result visually; his adjustments amend the commit concerned.

New logic gets unit or integration tests (`crates/*/tests/`), especially file formats and physics.

## Commits

- **Commit automatically**: each feature, fix or change gets its own commit as soon as its verification passes, without waiting to be asked; several subjects in one request give several commits. Message: a title, then a list of changes.
- Stage only the hunks of the commit's subject; when splitting a file, check that each hunk landed in the right commit and that each commit builds on its own. Never mix a line-ending conversion with a real change.
- **Adjustments** to a subject whose commit is not pushed: amend it (`git commit --amend` for the last one, otherwise `git commit --fixup=<sha>` then `GIT_SEQUENCE_EDITOR=: git rebase -i --autosquash <sha>~1`). A pushed commit gets a new commit instead.
- **Before starting a new subject**, if the working copy has uncommitted changes that are not mine, say so and ask whether to commit them first.
