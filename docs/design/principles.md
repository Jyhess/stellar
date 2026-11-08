# Design principles

Read before designing or changing code; each principle links to its details.

- **Errors are typed values.** Lower layers never return display text; the interface turns each error kind into its translated message. Details: [errors.md](errors.md).
- **Rules are guaranteed by structure or a failing test**, never by a comment or a reminder; the likely next change must not break silently. Details: [robustness.md](robustness.md).
