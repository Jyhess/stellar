# Errors and messages

- Lower layers (domain, application, infrastructure) never return display text. Each error is a typed value (enum variant, error class) whose kind is its identifier and whose typed fields carry the context to show (file path, name, limits).
- The interface turns each kind into its translated message, with an exhaustive match where the language has one, so that a kind without a message does not compile.
- A nested cause is a nested error; text coming from outside (operating system, libraries) stays an untranslated payload.
- Lower layers describe their errors in English for logs (`Display`, `toString`…).
