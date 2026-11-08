# Stellar — shared project instructions

3D gravitational N-body simulator in Rust (wgpu + egui) for Windows. Development machine: Windows, NVIDIA RTX 4060 Ti (8 GB dedicated), 16 GB of RAM.

Read before any substantial change: `SPEC.md` (rules in force), `README.md` (usage), and the page of `docs/` for the area (list in the README; `history.md` for the history of decisions).

**To find where to change something, start with `docs/code-map.md`** (files, key symbols, cross-layer change paths), then navigate by symbol with rust-analyzer. Keep the map up to date with the `code-map` skill.

## Communication and languages

- Talk to the user in **elaborate French with a rich vocabulary**.
- Do not try to prolong the discussion; ask a question only if it helps answer the request. Do ask every clarifying question **before** implementing a feature.
- **Documentation** (`*.md`, skills) is in **English**.
- **Code comments** (including doc comments and WGSL shaders), **CLI option names**, **commit messages** and **developer-facing text** (`expect` / `panic!` messages, tests, examples, `STELLAR_STATS` output) are in **English**.
- The **interface** has English source strings translated into French; errors of the lower layers are typed enums translated by the interface. Rules (`tr` / `tr!`, `errors.rs`, `format.rs`): the `new-feature` skill.

## Commands

```powershell
cargo build --release                      # target\release\stellar.exe and stellar-cli.exe
cargo test --workspace                     # GPU tests are skipped without a card
cargo clippy --workspace --all-targets
```

Toolchain: Rust **1.89** at least (`rust-version` in `Cargo.toml`, imposed by locked dependencies: `uuid`, `icu_*`). Quick starts (`STELLAR_AUTOSTART`, `STELLAR_LOAD`, `STELLAR_ENGINE`…), measurement tools and software rendering (`WGPU_BACKEND=gl`): `docs/testing.md`. Console: `stellar-cli.exe --help`; an unknown argument gives exit code 2.

## Documentation and tracking

- Documentation lives in `docs/`; no wiki. Tasks are GitHub issues of this repository.
- Time spent is recorded in the work journal (github.com/Jyhess/dev-journal).

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

## Delicate points

Read the page before touching its area; the traps that cost the most:

- **GPU engine** (`docs/gpu-engine.md`): data taken from `snapshot.state` stays consistent with `state.step`, not `snapshot.step` (the full state lags); accessors never read the device back; keep Mailbox presentation (`present_mode`) and Vulkan (DirectX 12 compiles shaders for ~100 s).
- **Sub-simulations** (`docs/subsystems.md`): the engine is always wrapped in `MultiSystem`; `state()` is in global coordinates.
- **Rendering and camera** (`docs/architecture.md`): positions relative to the anchor; instance buffers in chunks.
- **User files**: repositories delete to the recycle bin (`with_trash`), tests delete permanently in a temporary folder. Files in `saves\` and `recordings\` can weigh tens of gigabytes: never modify them for a test.

## File formats

Any field change of a saved or recorded structure needs a new version: **use the `file-format` skill**. Current versions and layout: `docs/formats.md`.

## Verification

1. **Before running tests**: check that no `stellar` / `stellar-cli` process is running (it shares the GPU and makes the GPU tests hang); if one is, ask the user to close it first.
2. **Before each commit** (see "Commits": every finished feature, fix or change is committed right away): `cargo test --workspace --exclude stellar-engine-gpu` and `cargo clippy --workspace --all-targets -- -D warnings`; the pre-push hook and CI run them again (`docs/testing.md`).
3. **Before handing over**: the GPU tests, one at a time (`cargo test -p stellar-engine-gpu -- --test-threads=1`; in parallel they hang on the shared card), then `cargo build --release` and copy `stellar.exe` and `stellar-cli.exe` from `target\release\` to the repository root, replacing the previous ones. No visual check on my side: the user tries it and validates; their adjustments then amend the commit concerned.

Add unit or integration tests (`crates/*/tests/`) for any new logic, especially file formats and physics.

## Commits

- Commit messages: a title, then a list of changes.
- Line endings: LF in the repository (`.gitattributes`, `eol=lf`). Files may be CRLF in the Windows working copy; Git normalizes them. Never commit a line-ending conversion mixed with a real change.
- **Commit automatically**: every feature, fix or change gets its own commit as soon as it is finished (checks of "Verification" passed), without waiting to be asked or for the user's validation. Several subjects in one request give several commits.
- Stage only the hunks of the commit's subject. When splitting a file's hunks, check that each staged hunk landed in the right place (one line of context can match elsewhere) and that each commit builds on its own.
- **Adjustments** requested on a subject whose commit is not pushed yet: amend that commit instead of adding a new one — `git commit --amend` if it is the last commit, otherwise `git commit --fixup=<sha>` then `GIT_SEQUENCE_EDITOR=: git rebase -i --autosquash <sha>~1` (non-interactive; `--autostash` if the working copy is dirty). A pushed commit is never amended: the adjustment gets a new commit.
- **Before starting a new subject**, if the working copy has uncommitted changes that are not mine, say so and ask whether to commit them first.

## Architecture

- Layer rules, crates and adding an engine: `docs/architecture.md`. The components and their allowed dependencies are in `docs/architecture.puml`, checked by `python3 .github/scripts/architecture.py` with the verification (`--write` rewrites them from the code).
- Changing the architecture file or these checks is an architecture change, approved by Julien before it is made (shared clarification workflow).

## Design

- Before designing or changing code, read `docs/design/principles.md` (errors, robustness by design).

## Shared rules

- Shared rules: `~/.dev-kit/rules/shared-rules.md`; Claude Code gets them from the core plugin, other agents read the file at session start; a section of this file that contradicts them wins.
