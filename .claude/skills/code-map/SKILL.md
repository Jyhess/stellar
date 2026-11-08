---
name: code-map
description: How to use and maintain docs/code-map.md, the map of where things live in Stellar's code (files, key symbols, change paths). Use at the start of any code change to locate the places to modify, and before handing over any change that adds, removes, renames or splits a source file or module, moves a responsibility, adds a public type or entry point, or reveals a change path that took searching to find.
---

# Code map

`docs/code-map.md` says where things live: one table per crate (file → contents, key symbols) and a "Where to change…" table of common change paths across layers.

## Using it

1. Before searching the code, read the "Where to change…" row that matches the task, then the tables of the crates concerned.
2. Jump to the named symbols with rust-analyzer (workspace symbol, go to definition, find references) or `grep -n "fn name"`; the map names symbols, never line numbers.
3. If the map was wrong or silent and you had to search, note it: it is the update to make at the end.

## Keeping it up to date

Update the map in the same change as the code, when the change:

| Change | Update |
|---|---|
| Adds, removes, renames or moves a source file (`.rs`, `.wgsl`) | the row in its crate's table (the test below enforces it) |
| Splits a file into a module directory | replace the row by one row per new file; move "Inside `x.rs`" groups accordingly |
| Adds or removes a public type, trait, engine method, `Command`, `Snapshot` field, screen or panel | the "Contents" cell of its file; the "Inside `sim.rs`" table for `SimScreen` methods |
| Moves a responsibility (e.g. a rule from an engine to the domain) | every row and path that names it |
| Adds an environment variable or a new kind of extension point | the corresponding "Where to change…" row |
| Required searching to find all the places to modify | add or complete a "Where to change…" row with the path found |

Rules:

- Name symbols (`Type::method`, `fn`), not line numbers; keep each cell to the essentials: what a reader needs to choose the file.
- Paths in a crate's table are relative to that crate's `src/`; shaders may be listed by name in backticks (`` `forces` ``).
- Do not duplicate the other pages of `docs/` or the README: link to them.
- English, like all documentation.

## Check

`cargo test -p stellar-app --test code_map` fails if a source file under `crates/*/src` is missing from its crate's section (`## <crate>`). It runs with `cargo test --workspace` before every commit. It does not check the descriptions: renamed symbols and moved responsibilities rely on the table above. When in doubt, grep the map for the old name:

```bash
grep -n "old_name" docs/code-map.md
```
