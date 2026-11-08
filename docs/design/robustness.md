# Robustness by design

- A rule the code must follow is guaranteed by its structure (types, compiler, a single source of truth) or by a failing test; never by a comment or an instruction to remember ("remember to…", "every property is copied here").
- Before choosing a design, picture its most likely change (one more property, case or type) and check what it would break. Prefer the design where that omission does not compile or cannot happen; otherwise, a test that finds new cases by itself (reflection, exhaustive `match`) and fails.
- No computed state in a copyable object (`record`, `struct`, value type): the computation lives apart, keyed by the instance.
- A regression test is proven by making it fail: reintroduce the defect it targets, see it fail, then remove the defect.
- A ticket's implementation notes follow the same rules.
