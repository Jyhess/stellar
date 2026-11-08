---
name: new-feature
description: End-to-end checklist for adding or changing a user-visible feature in Stellar — clarifying questions, English source strings with French translations, keyboard help, command-line help, README / SPEC updates, tests and the two-stage verification. Use whenever a request adds or changes behaviour the user can see (UI, controls, CLI options, simulation options).
---

# Adding a feature

## 1. Before coding

- Run `git status`: if there are uncommitted changes, say so and ask whether to commit them first (one commit per feature, see `AGENTS.md`).
- Read the relevant parts of `SPEC.md` and `README.md`, and the matching "Where to change…" row of `docs/code-map.md`.
- Ask **all** the clarifying questions at once, before implementing (ranges, defaults, keys, behaviour in playback / without display / with sub-simulations, compatibility of old saves). Ask in French.

## 2. Implementation rules

- **Interface text**: source strings are in English. Any displayed text goes through `tr("…")` or `tr!("… {} …", x)` (only `{}` placeholders) and gets its French translation in the `FR` table of `crates/stellar-app/src/i18n.rs`. Tests check duplicates, placeholders, and that every `tr` / `tr!` literal and engine label has its entry.
- **New error in a lower crate**: a variant of that layer's error enum (context as typed fields, English `Display`), never a `String`. Its text goes in the `Localized` impl of `stellar-app/src/errors.rs` (the exhaustive `match` does not compile without it) with its `FR` entry; display it with `e.localized()`.
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
| New environment variable | where it is read, `docs/testing.md` "Environment variables" table |
| New engine | registration in `crates/stellar-app/src/lib.rs`, `--engine` mapping in `console.rs` |
| Any feature | `SPEC.md` (rules in force), a line in the current or a new entry of `docs/history.md`, README sections concerned |
| New file, type, panel or change path | `docs/code-map.md` (`code-map` skill) |

Documentation, code comments and CLI option names are written in **English**; interface source strings are in **English** and translated into French; lower-layer errors are typed enums translated by the interface; developer-facing text (panics, tests, examples) is in **English**.

## 4. Tests

- Add unit or integration tests (`crates/*/tests/`) for any new logic, especially physics and file formats.
- Before committing: `cargo test --workspace` and `cargo clippy --workspace --all-targets`; fix any issue first.
- Before handing over: `cargo build --release` if the user is to try the app. No visual check on my side: the user tries it and validates.

## 5. Commit (automatic)

As soon as the feature is finished and checked, commit it without being asked: one commit per feature, fix or change, containing only its hunks (several subjects in one request give several commits). Adjustments to a subject whose commit is not pushed yet amend that commit (`--amend` for the last one, otherwise `--fixup` + autosquash rebase, see `AGENTS.md`); a pushed commit is never amended. English message: a title, then a list of changes, ending with the attribution line. Never mix a line-ending conversion with a real change. Push only when asked.
