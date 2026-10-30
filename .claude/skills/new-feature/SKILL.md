---
name: new-feature
description: End-to-end checklist for adding or changing a user-visible feature in Stellar — clarifying questions, French source strings with English translations, keyboard help, command-line help, README / SPEC updates, tests and the two-stage verification. Use whenever a request adds or changes behaviour the user can see (UI, controls, CLI options, simulation options).
---

# Adding a feature

## 1. Before coding

- Read the relevant parts of `SPEC.md` and `README.md`.
- Ask **all** the clarifying questions at once, before implementing (ranges, defaults, keys, behaviour in playback / without display / with sub-simulations, compatibility of old saves). Ask in French.

## 2. Implementation rules

- **Interface text**: source strings are in French. Any displayed text goes through `tr("…")` or `tr!("… {} …", x)` (only `{}` placeholders) and gets its English translation in the `EN` table of `crates/stellar-app/src/i18n.rs`. A test checks duplicates and placeholders.
- **Numbers**: use `format.rs` (`count`, `real`, `decimal_sep()`); never hard-code `.replace('.', ",")`.
- **Symbols**: some characters are missing from egui's fonts (●, ✦, U+202F). Check that a symbol is displayed before using it.
- **Layers**: no egui or wgpu in `stellar-domain` / `stellar-application`; engines are reached only through the domain traits.
- **Serialized types**: if a field is added to anything saved or recorded, follow the `file-format` skill.
- **Live GPU view**: data taken from `snapshot.state` must stay consistent with `state.step`, not with `snapshot.step` (the full state can lag behind).

## 3. Places to update

| Change | Update |
|---|---|
| New key | `sim.rs` (input handling and the key help list of the Esc menu), README "Controls" table |
| New panel / button / option | `home.rs` or `sim.rs`, `?` help badge if it is a creation parameter |
| New command-line option | `console.rs` (parser, `--help` text in `i18n.rs`, its tests), README "Simulation without display" |
| New environment variable | where it is read, README "Environment variables" table, `CLAUDE.md` if it is a testing shortcut |
| New engine | registration in `crates/stellar-app/src/lib.rs`, `--moteur` mapping in `console.rs` |
| Any feature | `SPEC.md` (rules in force), a line in the current or a new entry of `docs/history.md`, README sections concerned |

Documentation is written in **English**. Code comments, error messages of the lower layers, CLI options and commit messages stay in **French**.

## 4. Tests

- Add unit or integration tests (`crates/*/tests/`) for any new logic, especially physics and file formats.
- Before handing over: targeted tests only (`cargo test -p <crate>`), plus `cargo build --release` if the user is to try the app. No visual check on my side: the user tries it and validates.
- After the user validates, before committing: `cargo test --workspace` and `cargo clippy --workspace --all-targets`; fix any issue first.

## 5. Commit (only when asked)

French message: a title, then a list of changes, ending with the attribution line. Never mix a line-ending conversion with a real change.
