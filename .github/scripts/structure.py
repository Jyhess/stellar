#!/usr/bin/env python3
"""Find the structural changes of a pull request in its diff (rules: dev-kit's docs/pr-description.md).

Imported by pr_description.py, whose check fails when the description has a structural
change and no `## Architecture changes` section, and by auto_merge.py, which never
merges such a pull request. A change is structural when it:

- changes an architecture file: `docs/architecture.*`, `*ArchitectureTest*`,
  `.importlinter`, `.dependency-cruiser.*`, or a pattern of `architecture_patterns`;
- adds, removes or changes a manifest: `*.csproj`, `*.sln`, `package.json`,
  `pyproject.toml`, `requirements*.txt`, `Cargo.toml`, `go.mod`, `project.godot`,
  `*.gemspec`, `Gemfile`, or a pattern of `manifest_patterns` (lock files follow their
  manifest and do not count);
- adds or removes a top-level folder, hidden ones included;
- removes or modifies a component interface:
  - in C#, a `public interface` declaration (header or member changed, interface removed
    or renamed); moves between files are not structural;
  - in Python, a top-level function, class or constant of a module that another file
    imports (signature, bases, decorators or a public method changed; name removed,
    renamed or moved to another module; a constant's value does not count);
  - in Rust, a `pub trait` under a `src/` folder, keyed by `crate::module::Name` (header or
    member signature changed; trait removed, renamed or moved to another module; default
    method bodies do not count);
  - in Kotlin, a public `interface`, `fun interface` or `sealed interface`, as in C#,
    except that member bodies and private members do not count and string literals do;
  additions, comments and formatting are not structural.

Both pattern lists extend the defaults in .github/auto-merge.json, with the syntax of
`test_patterns` (see `matches`). Paths give every signal but interfaces, which are read
from the content of every non-test file of the extension, at the merge base and at head,
by one extractor per file extension (`INTERFACE_EXTRACTORS`).
"""

from __future__ import annotations

import ast
import copy
import enum
import json
import re
import subprocess
from collections import defaultdict
from dataclasses import dataclass
from fnmatch import fnmatchcase
from pathlib import Path, PurePosixPath

CONFIG = ".github/auto-merge.json"
DEFAULT_ARCHITECTURE_PATTERNS = ["docs/architecture.*", "*ArchitectureTest*", ".importlinter", ".dependency-cruiser.*",
                                 ".github/scripts/architecture.py"]
DEFAULT_MANIFEST_PATTERNS = ["*.csproj", "*.sln", "package.json", "pyproject.toml", "requirements*.txt",
                             "Cargo.toml", "go.mod", "project.godot", "*.gemspec", "Gemfile"]
DEFAULT_TEST_PATTERNS = ["tests/", "test_*", "*_test.*", "*Test.cs", "*Tests.cs", "*.spec.*"]


class Kind(enum.Enum):
    ARCHITECTURE_FILE = "architecture file changed"
    MANIFEST_ADDED = "manifest added"
    MANIFEST_REMOVED = "manifest removed"
    MANIFEST_CHANGED = "manifest changed"
    FOLDER_ADDED = "top-level folder added"
    FOLDER_REMOVED = "top-level folder removed"
    INTERFACE_CHANGED = "public interface changed"
    INTERFACE_REMOVED = "public interface removed"


@dataclass(frozen=True)
class Signal:
    kind: Kind
    path: str
    detail: str = ""

    def __str__(self) -> str:
        return f"{self.path}: {self.kind.value}" + (f" ({self.detail})" if self.detail else "")


def load_config(path: Path) -> dict:
    if not path.is_file():
        return {}
    return json.loads(path.read_text(encoding="utf-8"))


def matches(path: str, pattern: str) -> bool:
    """A pattern ending in `/` is a folder name anywhere in the path, a pattern with a `/`
    is matched against the whole path, any other against the file name."""
    if pattern.endswith("/"):
        return pattern[:-1] in path.split("/")[:-1]
    if "/" in pattern:
        return fnmatchcase(path, pattern)
    return fnmatchcase(path.rsplit("/", 1)[-1], pattern)


def test_patterns(config: dict) -> list[str]:
    return DEFAULT_TEST_PATTERNS + list(config.get("test_patterns", []))


def signals(changes: list[tuple[str, str]], base_folders: set[str], head_folders: set[str],
            config: dict) -> list[Signal]:
    """Structural changes among the changed files (git status letter, path) and the top-level folders."""
    architecture = DEFAULT_ARCHITECTURE_PATTERNS + list(config.get("architecture_patterns", []))
    manifests = DEFAULT_MANIFEST_PATTERNS + list(config.get("manifest_patterns", []))
    found = []
    for status, path in changes:
        if any(matches(path, p) for p in architecture):
            found.append(Signal(Kind.ARCHITECTURE_FILE, path))
        elif any(matches(path, p) for p in manifests):
            kind = {"A": Kind.MANIFEST_ADDED, "D": Kind.MANIFEST_REMOVED}.get(status, Kind.MANIFEST_CHANGED)
            found.append(Signal(kind, path))
    found += [Signal(Kind.FOLDER_ADDED, f"{name}/") for name in sorted(head_folders - base_folders)]
    found += [Signal(Kind.FOLDER_REMOVED, f"{name}/") for name in sorted(base_folders - head_folders)]
    return found


# An interface: its normalized header and the set of its normalized members.
Interface = tuple[str, frozenset[str]]


@dataclass(frozen=True)
class Declared:
    """An interface found by an extractor."""
    path: str
    name: str  # qualified name shown in the signal
    interface: Interface
    exposed: bool = True  # part of a component interface; only exposed interfaces of the merge base are compared


@dataclass(frozen=True)
class Extraction:
    """What an extractor finds in a set of files: interfaces by identity key, and the files it could not parse,
    whose interfaces count as changed."""
    interfaces: dict[str, Declared]
    unreadable: frozenset[str] = frozenset()

CSHARP_NOISE = re.compile(r"""//[^\n]*|/\*.*?\*/|@"(?:[^"]|"")*"|\$?"(?:\\.|[^"\\\n])*"|'(?:\\.|[^'\\\n])*'""",
                          re.DOTALL)
CSHARP_NAMESPACE = re.compile(r"\bnamespace\s+([\w.]+)\s*([;{])")
CSHARP_INTERFACE = re.compile(r"\binterface\s+(\w+)")
CSHARP_ATTRIBUTES = re.compile(r"\[[^\]]*\]")


def normalize(code: str) -> str:
    """Code with its whitespace collapsed and none around punctuation."""
    return re.sub(r"\s*([(){}\[\],;<>:=?])\s*", r"\1", " ".join(code.split()))


def closing_brace(text: str, opening: int) -> int:
    """Index of the brace closing the one at opening (the end of text when unbalanced)."""
    depth = 0
    for index in range(opening, len(text)):
        depth += {"{": 1, "}": -1}.get(text[index], 0)
        if depth == 0:
            return index
    return len(text)


def members(body: str) -> frozenset[str]:
    """Members of an interface body: each ends at a `;` or a closing brace at depth 0."""
    found, depth, start = set(), 0, 0
    for index, char in enumerate(body):
        depth += {"{": 1, "}": -1}.get(char, 0)
        if depth == 0 and char in ";}":
            found.add(normalize(body[start:index + 1]))
            start = index + 1
    return frozenset(found - {""})


def csharp_interfaces(text: str) -> dict[str, Interface]:
    """The `public interface` declarations of a C# file, by namespace-qualified name."""
    text = CSHARP_NOISE.sub(lambda m: " " if m.group().startswith("/") else '""', text)
    text = "\n".join("" if line.lstrip().startswith("#") else line for line in text.split("\n"))
    namespaces = [(m.start(), len(text) if m.group(2) == ";" else closing_brace(text, m.end() - 1), m.group(1))
                  for m in CSHARP_NAMESPACE.finditer(text)]
    found = {}
    for match in CSHARP_INTERFACE.finditer(text):
        start = max(text.rfind(c, 0, match.start()) for c in ";{}") + 1
        if "public" not in CSHARP_ATTRIBUTES.sub(" ", text[start:match.start()]).split():
            continue
        end = min((i for i in (text.find("{", match.end()), text.find(";", match.end())) if i >= 0), default=-1)
        if end < 0:
            continue
        body = members(text[end + 1:closing_brace(text, end)]) if text[end] == "{" else frozenset()
        namespace = ".".join(name for first, last, name in namespaces if first < match.start() < last)
        found[f"{namespace}.{match.group(1)}" if namespace else match.group(1)] = (normalize(text[start:end]), body)
    return found


def csharp_files(files: dict[str, str]) -> Extraction:
    """The `public interface` declarations of C# files, keyed by qualified name, so a move between files is no change."""
    return Extraction({name: Declared(path, name, interface) for path, text in sorted(files.items())
                       for name, interface in csharp_interfaces(text).items()})


def python_signature(node: ast.stmt) -> str:
    """A `def` or `class` statement with its decorators and without its body, docstring included."""
    stub = copy.copy(node)
    stub.body = [ast.Pass()]
    return ast.unparse(stub)


def assigned_names(target: ast.expr) -> list[str]:
    if isinstance(target, ast.Name):
        return [target.id]
    if isinstance(target, (ast.Tuple, ast.List)):
        return [name for element in target.elts for name in assigned_names(element)]
    return []


def python_members(node: ast.ClassDef) -> frozenset[str]:
    """Public methods (signature, `__init__` included) and class-body attributes (`name: annotation`, or `name`
    when unannotated: their value is not part of the interface) of a class."""
    found = set()
    for member in node.body:
        if isinstance(member, (ast.FunctionDef, ast.AsyncFunctionDef)):
            if not member.name.startswith("_") or member.name == "__init__":
                found.add(python_signature(member))
        elif isinstance(member, ast.AnnAssign) and isinstance(member.target, ast.Name):
            if not member.target.id.startswith("_"):
                found.add(f"{member.target.id}: {ast.unparse(member.annotation)}")
        elif isinstance(member, ast.Assign):
            found.update(name for target in member.targets for name in assigned_names(target)
                         if not name.startswith("_"))
    return frozenset(found)


def python_definitions(tree: ast.Module) -> dict[str, Interface]:
    """Top-level functions (signature), classes (header and members), constants (name only: their value is not
    part of the interface) and names re-exported by `from a import x [as y]` (name only: their definition is `a`'s
    interface) of a module; the last binding of a name wins, as at run time."""
    found = {}
    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            found[node.name] = (python_signature(node), frozenset())
        elif isinstance(node, ast.ClassDef):
            found[node.name] = (python_signature(node), python_members(node))
        elif isinstance(node, (ast.Assign, ast.AnnAssign)):
            for target in node.targets if isinstance(node, ast.Assign) else [node.target]:
                found.update((name, ("", frozenset())) for name in assigned_names(target))
        elif isinstance(node, ast.ImportFrom) and node.module:
            found.update((alias.asname or alias.name, ("", frozenset())) for alias in node.names if alias.name != "*")
    return found


def star_names(tree: ast.Module, definitions: dict[str, Interface]) -> set[str]:
    """What `from module import *` imports: `__all__` when the module declares it, else the names without `_`."""
    for node in tree.body:
        if (isinstance(node, ast.Assign) and isinstance(node.value, (ast.List, ast.Tuple))
                and any("__all__" in assigned_names(target) for target in node.targets)):
            return {e.value for e in node.value.elts if isinstance(e, ast.Constant) and isinstance(e.value, str)}
    return {name for name in definitions if not name.startswith("_")}


def dotted(node: ast.expr) -> str | None:
    if isinstance(node, ast.Name):
        return node.id
    if isinstance(node, ast.Attribute):
        prefix = dotted(node.value)
        return f"{prefix}.{node.attr}" if prefix else None
    return None


def python_imports(tree: ast.Module) -> list[tuple[str, str]]:
    """(module name without its package, imported name or `*`) of `from m import X`, and of `m.X` after `import m`."""
    found, modules = [], {}
    for node in ast.walk(tree):
        if isinstance(node, ast.ImportFrom) and node.module:
            found += [(node.module.rsplit(".", 1)[-1], alias.name) for alias in node.names]
        elif isinstance(node, ast.Import):
            modules.update((alias.asname or alias.name, alias.name.rsplit(".", 1)[-1]) for alias in node.names)
    for node in ast.walk(tree):
        if isinstance(node, ast.Attribute) and dotted(node.value) in modules:
            found.append((modules[dotted(node.value)], node.attr))
    return found


def module_name(path: str) -> str:
    """The name a file is imported by: its stem, or its folder for a package's `__init__.py`."""
    file = PurePosixPath(path)
    return file.parent.name if file.stem == "__init__" else file.stem


def python_interfaces(files: dict[str, str]) -> Extraction:
    """The top-level definitions of Python files, keyed by file and name, exposed when another file imports them.

    An import resolves to every file of that module name, in any folder, since scripts extend `sys.path`."""
    trees, unreadable = {}, set()
    for path, text in sorted(files.items()):
        try:
            trees[path] = ast.parse(text)
        except (SyntaxError, ValueError):
            unreadable.add(path)
    definitions = {path: python_definitions(tree) for path, tree in trees.items()}
    by_module = defaultdict(list)
    for path in trees:
        by_module[module_name(path)].append(path)
    imported = defaultdict(set)
    for importer, tree in trees.items():
        for module, name in python_imports(tree):
            for path in by_module.get(module, []):
                if path != importer:
                    imported[path] |= star_names(trees[path], definitions[path]) if name == "*" else {name}
    return Extraction({f"{path}:{name}": Declared(path, f"{module_name(path)}.{name}", interface, name in imported[path])
                       for path, found in definitions.items() for name, interface in found.items()},
                      frozenset(unreadable))


RUST_NOISE = re.compile(r"""//[^\n]*|/\*.*?\*/|b?r(#*)".*?"\1|"(?:\\.|[^"\\])*"|'(?:\\(?:u\{[0-9a-fA-F]*\}|.)|[^'\\\n])'""",
                        re.DOTALL)
RUST_MODULE = re.compile(r"\bmod\s+(\w+)\s*\{")
RUST_TRAIT = re.compile(r"\btrait\s+(\w+)")
RUST_ATTRIBUTES = re.compile(r"#!?\[[^\]]*\]")
RUST_PUBLIC = re.compile(r"\bpub\b(?!\s*\()")


def rust_module(path: str) -> list[str] | None:
    """Crate and module path of a Rust file: the folder holding `src/`, then the path under it
    (`src/lib.rs`, `src/main.rs` and `mod.rs` stand for their parent module); None outside a `src/`
    folder. A crate at the repository root is `crate`."""
    *folders, file = path.split("/")
    if "src" not in folders:
        return None
    index = len(folders) - 1 - folders[::-1].index("src")
    modules = folders[index + 1:]
    name = file.removesuffix(".rs")
    if name != "mod" and (modules or name not in ("lib", "main")):
        modules.append(name)
    return [folders[index - 1] if index else "crate", *modules]


def rust_members(body: str) -> frozenset[str]:
    """Signatures of a trait body: each item ends at a `;` or at its default body, which is dropped."""
    found, depth, start = set(), 0, 0
    for index, char in enumerate(body):
        if depth == 0 and char in ";{":
            found.add(normalize(body[start:index]))
        depth += {"{": 1, "}": -1}.get(char, 0)
        if depth == 0 and char in ";}":
            start = index + 1
    return frozenset(found - {""})


def rust_traits(path: str, text: str) -> dict[str, Interface]:
    """The `pub trait` declarations of a Rust file, by `crate::module::Name`.

    Known limitations: a `pub trait` in a private module still counts, `pub use` re-exports, traits
    generated by macros and `#[path]` attributes are ignored."""
    module = rust_module(path)
    if module is None:
        return {}
    text = RUST_NOISE.sub(lambda m: " " if m.group().startswith("/") else '""', text)
    modules = [(m.start(), closing_brace(text, m.end() - 1), m.group(1)) for m in RUST_MODULE.finditer(text)]
    found = {}
    for match in RUST_TRAIT.finditer(text):
        start = max(text.rfind(c, 0, match.start()) for c in ";{}") + 1
        if not RUST_PUBLIC.search(RUST_ATTRIBUTES.sub(" ", text[start:match.start()])):
            continue
        end = min((i for i in (text.find("{", match.end()), text.find(";", match.end())) if i >= 0), default=-1)
        if end < 0:
            continue
        body = rust_members(text[end + 1:closing_brace(text, end)]) if text[end] == "{" else frozenset()
        inline = [name for first, last, name in modules if first < match.start() < last]
        found["::".join([*module, *inline, match.group(1)])] = (normalize(text[start:end]), body)
    return found


def rust_files(files: dict[str, str]) -> Extraction:
    """The `pub trait` declarations of Rust files, keyed by `crate::module::Name`, so a move between files of
    the same module is no change."""
    return Extraction({name: Declared(path, name, interface) for path, text in sorted(files.items())
                       for name, interface in rust_traits(path, text).items()})


KOTLIN_NOISE = re.compile(r'''""".*?"""|"(?:\\.|[^"\\\n])*"|'(?:\\.|[^'\\\n])*'|//[^\n]*|/\*.*?\*/''',
                          re.DOTALL)
KOTLIN_STRING = re.compile(r"\x00(\d+)\x00")
KOTLIN_PACKAGE = re.compile(r"^\s*package\s+([\w.]+)", re.MULTILINE)
KOTLIN_TYPE = re.compile(r"(?<!::)\b(class|interface|object)\s+(\w+)")
KOTLIN_MODIFIERS = ("public|internal|private|protected|sealed|abstract|open|override|fun|suspend|inline|data|enum"
                    "|annotation|inner|value|expect|actual|external|operator|infix|tailrec|const|lateinit|companion")
# The annotations and modifiers just before a declaration keyword.
KOTLIN_PREFIX = re.compile(rf"(?:(?:@[\w.:]+(?:\([^()]*\))?|\b(?:{KOTLIN_MODIFIERS}))\s*)*\Z")
# A line that starts a new declaration, so the declaration before it has no body.
KOTLIN_DECLARATION = re.compile(rf"\s*(?:@|\b(?:{KOTLIN_MODIFIERS}|val|var|class|interface|object|typealias)\b)")
KOTLIN_MEMBER = re.compile(r"\b(fun|val|var|class|interface|object)\b")
KOTLIN_SIGNATURE_END = re.compile(r"[{=]|\b(?:get|set)\b")


def kotlin_normalize(code: str) -> str:
    """Normalized code without trailing commas, which Kotlin allows before a closing `)`, `>` or `]`."""
    return re.sub(r",([)>\]])", r"\1", normalize(code))


def kotlin_prefix_start(text: str, keyword: int) -> int:
    """Start of the annotations and modifiers before the declaration keyword at keyword, when they open a line."""
    start = KOTLIN_PREFIX.search(text, 0, keyword).start()
    before = text[:start].rstrip(" \t")
    return start if not before or before[-1] in "\n{};" else keyword


def kotlin_header_end(text: str, start: int) -> int:
    """Index of the `{` opening the body of the declaration at start, or of where it ends without a body."""
    depth = 0
    for index in range(start, len(text)):
        char = text[index]
        depth += {"(": 1, ")": -1, "<": 1, ">": -1}.get(char, 0)
        if depth > 0:
            continue
        if char in "{}" or char == "\n" and KOTLIN_DECLARATION.match(text, index + 1):
            return index
    return len(text)


def kotlin_members(body: str) -> frozenset[str]:
    """Signatures of the non-private `fun`, `val` and `var` members of an interface body, without their bodies;
    nested types are left to their own declaration."""
    keywords, depth, index = [], 0, 0
    while index < len(body):
        depth += {"{": 1, "}": -1, "(": 1, ")": -1, "[": 1, "]": -1}.get(body[index], 0)
        match = KOTLIN_MEMBER.match(body, index) if depth == 0 else None
        if match and (index == 0 or not (body[index - 1].isalnum() or body[index - 1] in "_.")):
            if not (match.group(1) == "fun" and body[match.end():].lstrip().startswith("interface")):
                keywords.append((kotlin_prefix_start(body, index), match))
            index = match.end()
            continue
        index += 1
    found = set()
    for (start, match), (end, _) in zip(keywords, keywords[1:] + [(len(body), None)]):
        if match.group(1) not in ("fun", "val", "var") or "private" in body[start:match.start()].split():
            continue
        signature, depth = body[start:end], 0
        for index, char in enumerate(signature):
            depth += {"(": 1, ")": -1, "[": 1, "]": -1}.get(char, 0)
            if depth == 0 and index >= match.start() - start and KOTLIN_SIGNATURE_END.match(signature, index):
                signature = signature[:index]
                break
        found.add(kotlin_normalize(signature))
    return frozenset(found)


def kotlin_interfaces(text: str) -> dict[str, Interface]:
    """The public `interface`, `fun interface` and `sealed interface` declarations of a Kotlin file, by
    package-qualified name (nested ones under their enclosing types); member bodies and private members do not
    count, string literals do."""
    strings = []

    def mask(match: re.Match) -> str:
        if match.group().startswith("/"):
            return " "
        strings.append(match.group())
        return f"\x00{len(strings) - 1}\x00"

    def unmask(code: str) -> str:
        return KOTLIN_STRING.sub(lambda m: strings[int(m.group(1))], code)

    # `public` is the default visibility, so writing it changes nothing.
    text = re.sub(r"\bpublic\b", " ", KOTLIN_NOISE.sub(mask, text))
    package = KOTLIN_PACKAGE.search(text)
    types = []
    for match in KOTLIN_TYPE.finditer(text):
        end = kotlin_header_end(text, match.end())
        start = kotlin_prefix_start(text, match.start())
        types.append((match, start, end, closing_brace(text, end) if end < len(text) and text[end] == "{" else end))
    found = {}
    for match, start, end, last in types:
        if match.group(1) != "interface":
            continue
        if {"internal", "private", "protected"} & set(text[start:match.start()].split()):
            continue
        outer = [m.group(2) for m, _, opening, closing in types if opening < match.start() < closing]
        name = ".".join(([package.group(1)] if package else []) + outer + [match.group(2)])
        body = kotlin_members(text[end + 1:last]) if last > end else frozenset()
        found[name] = (unmask(kotlin_normalize(text[start:end])), frozenset(unmask(member) for member in body))
    return found


def kotlin_files(files: dict[str, str]) -> Extraction:
    """The public interfaces of Kotlin files, keyed by qualified name, so a move between files is no change."""
    return Extraction({name: Declared(path, name, interface) for path, text in sorted(files.items())
                       for name, interface in kotlin_interfaces(text).items()})


INTERFACE_EXTRACTORS = {".cs": csharp_files, ".py": python_interfaces, ".rs": rust_files, ".kt": kotlin_files}


def interface_signals(base: dict[str, str], head: dict[str, str]) -> list[Signal]:
    """Interfaces removed or modified between the base and head content {path: text} of the source files.

    An interface exposed at the base is removed when its key is gone at head, changed when its header changed,
    a member is gone or its file no longer parses; added interfaces and added members are not structural."""
    found = []
    for extension, extract in INTERFACE_EXTRACTORS.items():
        before = extract({path: text for path, text in base.items() if path.endswith(extension)})
        after = extract({path: text for path, text in head.items() if path.endswith(extension)})
        for key, declared in before.interfaces.items():
            if not declared.exposed:
                continue
            header, old_members = declared.interface
            if key not in after.interfaces:
                kind = Kind.INTERFACE_CHANGED if declared.path in after.unreadable else Kind.INTERFACE_REMOVED
                found.append(Signal(kind, declared.path, declared.name))
            elif after.interfaces[key].interface[0] != header or old_members - after.interfaces[key].interface[1]:
                found.append(Signal(Kind.INTERFACE_CHANGED, declared.path, declared.name))
    return found


def git(*args: str, cwd: Path | None = None) -> str:
    return subprocess.run(["git", "-c", "core.quotePath=false", *args], cwd=cwd, check=True,
                          capture_output=True, text=True, encoding="utf-8", errors="replace").stdout


def name_status(base: str, head: str, cwd: Path | None = None) -> list[tuple[str, str]]:
    """(status letter, path) of the files changed by head since its merge base with base."""
    listing = git("diff", "--name-status", "-z", "--no-renames", f"{base}...{head}", cwd=cwd).split("\0")
    return [(status[:1], path) for status, path in zip(listing[0::2], listing[1::2])]


def top_folders(ref: str, cwd: Path | None = None) -> set[str]:
    return set(git("ls-tree", "-d", "-z", "--name-only", ref, cwd=cwd).split("\0")) - {""}


def sources(ref: str, extensions: tuple[str, ...], tests: list[str], cwd: Path | None = None) -> dict[str, str]:
    """{path: text} of the files of ref with one of the extensions and matching no test pattern."""
    if not extensions:
        return {}
    listing = git("ls-tree", "-r", "-z", ref, cwd=cwd).split("\0")
    paths = [entry.split("\t", 1)[1] for entry in listing if entry and entry.split()[1] == "blob"]
    paths = [p for p in paths if p.endswith(extensions) and not any(matches(p, t) for t in tests)]
    if not paths:
        return {}
    batch = subprocess.run(["git", "cat-file", "--batch"], cwd=cwd, check=True, capture_output=True,
                           input="".join(f"{ref}:{path}\n" for path in paths).encode("utf-8")).stdout
    files, position = {}, 0
    for path in paths:
        header_end = batch.index(b"\n", position)
        size = int(batch[position:header_end].split()[2])
        files[path] = batch[header_end + 1:header_end + 1 + size].decode("utf-8", errors="replace")
        position = header_end + 1 + size + 1
    return files


def diff_signals(base: str, head: str, config: dict, cwd: Path | None = None) -> list[Signal]:
    """Structural changes of head since its merge base with base. Interfaces are read only for the extensions
    of the changed files, from every non-test file of those extensions."""
    merge_base = git("merge-base", base, head, cwd=cwd).strip()
    changes = name_status(base, head, cwd)
    extensions = tuple(e for e in INTERFACE_EXTRACTORS if any(path.endswith(e) for _, path in changes))
    tests = test_patterns(config)
    return (signals(changes, top_folders(merge_base, cwd), top_folders(head, cwd), config)
            + interface_signals(sources(merge_base, extensions, tests, cwd), sources(head, extensions, tests, cwd)))
