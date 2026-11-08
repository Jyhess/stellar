#!/usr/bin/env python3
"""Check that every component of the code is declared in the architecture file.

    python3 .github/scripts/architecture.py [--write] [repository dir]    # .NET, Kotlin, Rust, JS/TS
    <runner> python .github/scripts/architecture.py [--write] [repository dir]   # Python

The stack is read from the `stack:` header line of its architecture file (STACK_FILES),
the `stack` key of docs/architecture.json; a diagram without one is .NET.

.NET: a component is a namespace right under the root namespace, sub-namespaces included
(`Shop.Orders` holds `Shop.Orders.Lines`); types declared in the root namespace itself
form the root component. The diagram header gives the root and the folders to skip:

    ' root: Shop
    ' exclude: tests/ tools/

The diagram declares a component as `[Shop.Orders] <<^Shop\\.Orders(\\..+)?$>>` (see
`stereotype`); ArchitectureTest.cs checks its arrows with ArchUnitNET, which ignores
the types of undeclared namespaces, hence this check. It fails when a namespace of the
code is not declared, a declared component has no code, a stereotype is not the one
`stereotype` gives, a namespace is outside the root, or a `.cs` file has no namespace.
Hidden folders, `bin/` and `obj/` are skipped. `--write` rewrites the components and
their stereotypes from the namespaces of the code, dropping the arrows of a component
gone; `ARCHITECTURE_DIAGRAM=write dotnet test` then rewrites the arrows.

Kotlin: as .NET, with the packages of the `.kt` and `.java` files under the
folders of the `' sources:` header line (`build/` and hidden folders skipped), and an
ArchUnit package identifier as stereotype: `[com.shop] <<com.shop>>` for the root package
alone, `[com.shop.data] <<com.shop.data..>>` for a component and its sub-packages.
ArchitectureTest.kt checks the arrows with ArchUnit; `ARCHITECTURE_DIAGRAM=write` on the
unit tests rewrites them.

    ' stack: kotlin
    ' root: com.shop
    ' sources: app/src/main/

Python: a component is a direct child of the root package (`src/<root>/` or `<root>/`),
a sub-package with its sub-modules or a module; the root package's own `__init__` is the
component named after the root (`shop`), which `from shop import X` imports when X is not
a sub-module. `.importlinter` declares each one as an `allowed` contract (AllowedContract,
registered through `contract_types`, section `importlinter:contract:__init__` for the
root) that lists the components it may import directly. With several root packages
(`root_packages`), the components of all of them form one graph (an import from one root
package to another must be allowed) and each section is named after its component
(`importlinter:contract:shop.orders`, `importlinter:contract:shop`). A folder holding Python code but
no __init__.py, anywhere under the root package, fails the check: grimp, so import-linter,
skips it and everything under it.
The check fails when a child is not declared, a declared component has no code, or a
component imports one it does not allow (import-linter's
report, run in-process: import-linter must be installed in the project environment).
`--write` rewrites the contracts from the imports found today by `ast`, counted as grimp
counts them (relative, inside functions, under TYPE_CHECKING); it keeps the other options
of [importlinter] and the contracts of other types.

Rust: a component is a crate of the Cargo workspace (`cargo metadata --no-deps`), except
the crates named on the `' exclude:` header line. The diagram declares each one as
`[name]`, `[name] <<restricted>>` for a crate that may depend directly only on the
external crates drawn, declared as `[name] <<external>>`; an arrow `[a] --> [b]` (any
PlantUML arrow: `->`, `..>`, `-up->`) allows a dependency. Normal and build dependencies
count, dev ones do not; an arrow drawn but unused passes. The check fails when a crate
is not declared, a declared crate does not exist, a dependency between crates or an
external dependency of a restricted crate is not drawn, or an arrow names an undeclared
component. Other PlantUML forms (`component x`, `as` aliases) are not read: a crate
declared that way is reported as not declared. `--write` rewrites the components and arrows from the code, keeping the
header, the restricted crates and the other lines of the diagram.

JS/TS: docs/architecture.json lists the `roots` (folders, `.` for the repository) and the
folders to `exclude`; a component is a folder right under a root (`src/hooks`), sub-folders
included; the source files directly in a root form the component named after the root.
`components` maps each one to the components it may import. node_modules, hidden folders
and test files (JS_TESTS) are not components and are not checked. The imports come from
dependency-cruiser (`depcruise <roots> --config .dependency-cruiser.cjs`, from the project's
node_modules/.bin, else the PATH). The check fails when a component of the code is not
declared, a declared component has no code, a component allows an undeclared one, a file
imports a component its component does not allow, or dependency-cruiser did not read a
source file (TypeScript without a compiler it supports: its imports would pass unchecked).
`--write` rewrites `components` from the imports found today, keeping the other keys.

Every stack: the allowed dependencies between components must not form a cycle (a set of
components each reaching the others), since the code can only use the allowed ones. A known
cycle is admitted by naming exactly its components, one line per cycle: `' allowed cycle:
Shop.A Shop.B` in the diagram header, `# allowed cycle: shop.a shop.b` at the top of
.importlinter, `"allowed_cycles": [["src/a", "src/b"]]` in docs/architecture.json. The check
fails on a cycle not admitted, shown as a path through all its components, and on an
admitted cycle that no longer exists.
"""

from __future__ import annotations

import ast
import configparser
import enum
import json
import os
import re
import shutil
import subprocess
import sys
from collections.abc import Callable, Iterable
from dataclasses import dataclass
from pathlib import Path

DIAGRAM = "docs/architecture.puml"
IMPORTLINTER = ".importlinter"
ARCHITECTURE_JSON = "docs/architecture.json"
# .NET last: a diagram without a stack line is .NET.
STACK_FILES = {"python": IMPORTLINTER, "rust": DIAGRAM, "js": ARCHITECTURE_JSON, "kotlin": DIAGRAM,
               "dotnet": DIAGRAM}
STACK = re.compile(r"(?m)^\s*[;'#]\s*stack:\s*(\S+)")
SKIPPED = {"bin", "obj"}
KOTLIN_SKIPPED = {"build"}
NAMESPACE = re.compile(r"(?m)^\s*namespace\s+([A-Za-z_][\w.]*)")
PACKAGE = re.compile(r"(?m)^\s*package\s+([A-Za-z_][\w.]*)")
COMPONENT = re.compile(r"(?m)^\s*\[([^\]]+)\]\s*<<(.*)>>\s*$")
HEADER = re.compile(r"(?m)^'\s*(root|exclude|sources):(.*)$")
CONTRACT_SECTION = "importlinter:contract:"
CONTRACT_TYPE = "allowed"
PY_SKIPPED = {"__pycache__"}
RESTRICTED = "restricted"
EXTERNAL = "external"
DECLARATION = re.compile(r"^\s*\[([^\]]+)\]\s*(?:<<\s*(\w+)\s*>>)?\s*$")
ARROW = re.compile(r"^\s*\[([^\]]+)\]\s*[-.]+(?:\w+[-.]+)?>\s*\[([^\]]+)\]\s*$")
COUNTED_KINDS = {None, "build"}  # dependency kinds of cargo metadata that count: normal and build, not dev
CARGO_METADATA = ["cargo", "metadata", "--no-deps", "--format-version", "1"]
DEPCRUISE_CONFIG = ".dependency-cruiser.cjs"
JS_SOURCES = {".js", ".mjs", ".cjs", ".jsx", ".ts", ".mts", ".cts", ".tsx"}
JS_SKIPPED = {"node_modules"}
# Test files, neither components nor checked: they may import any component (provisional).
JS_TESTS = re.compile(r"(^|/)(__tests__|test|tests)/|\.(test|spec)\.[cm]?[jt]sx?$")
ALLOWED_CYCLE = re.compile(r"(?m)^\s*[;'#]\s*allowed cycle:(.*)$")
ALLOWED_CYCLES = "allowed_cycles"


class Kind(enum.Enum):
    UNDECLARED = "namespace not declared in " + DIAGRAM
    ABSENT = "component declared but no code in it"
    OUTSIDE_ROOT = "namespace outside the root namespace"
    NO_NAMESPACE = "file without namespace"
    NO_DIAGRAM = "diagram missing"
    NO_ROOT = "no `' root: <namespace or package>` line in the diagram header"
    NO_SOURCES = "no `' sources: <folders>` line in the diagram header"
    UNDECLARED_PACKAGE = "package not declared in " + DIAGRAM
    OUTSIDE_ROOT_PACKAGE = "package outside the root package"
    NO_PACKAGE_LINE = "file without package"
    UNDECLARED_MODULE = "component not declared in " + IMPORTLINTER
    NO_ROOT_PACKAGE = "no `root_package` or `root_packages` in the [importlinter] section"
    NO_PACKAGE = "root package not found (src/<root>/__init__.py or <root>/__init__.py)"
    NO_INIT = "folder without __init__.py: import-linter does not check the code under it, add one"
    NO_IMPORT_LINTER = "import-linter is not installed in this environment"
    UNDECLARED_CRATE = "crate not declared in " + DIAGRAM
    UNDRAWN_DEPENDENCY = "dependency not drawn in " + DIAGRAM
    UNDRAWN_EXTERNAL = "external dependency of a <<restricted>> crate not drawn in " + DIAGRAM
    UNKNOWN_ARROW = "arrow to or from a component not declared"
    NO_CARGO = "cargo not found"
    CARGO_FAILED = "cargo metadata failed"
    INVALID_FILE = "unreadable architecture file"
    NO_ROOTS = "no `roots` in " + ARCHITECTURE_JSON
    UNDECLARED_FOLDER = "component not declared in " + ARCHITECTURE_JSON
    UNKNOWN_ALLOWED = "allowed component not declared"
    IMPORT_NOT_ALLOWED = "import not allowed in " + ARCHITECTURE_JSON
    NO_DEPCRUISE = "dependency-cruiser not installed (npm install --save-dev dependency-cruiser)"
    DEPCRUISE_FAILED = "dependency-cruiser failed"
    NOT_READ = "source not read by dependency-cruiser (TypeScript needs the typescript or swc package it supports)"
    STALE_STEREOTYPE = "stereotype differs from the one --write gives"
    NO_STARTUML = "no @startuml line in the diagram"
    CYCLE = "cycle between components"
    STALE_CYCLE = "allowed cycle not found"


@dataclass(frozen=True)
class Problem:
    kind: Kind
    name: str
    path: str

    def __str__(self) -> str:
        return ": ".join(part for part in (self.path, self.name, self.kind.value) if part)


def named_stacks(file: Path) -> list[str]:
    """The stacks an architecture file names: its `stack` key for JSON, its `stack:` header lines otherwise."""
    text = file.read_text(encoding="utf-8")
    if file.suffix != ".json":
        return STACK.findall(text)
    try:
        content = json.loads(text)
    except ValueError:
        return []
    return [content.get("stack")] if isinstance(content, dict) else []


def stack(repo: Path) -> str:
    """The stack whose architecture file names it; .NET for a diagram without one."""
    for name, path in STACK_FILES.items():
        file = repo / path
        if file.is_file() and name in named_stacks(file):
            return name
    return "dotnet"


# Cycles, every stack: on the allowed dependencies between components.

def header_cycles(text: str) -> list[tuple[str, ...]]:
    """The cycles admitted by the `allowed cycle:` comment lines of an architecture file."""
    return [tuple(line.split()) for line in ALLOWED_CYCLE.findall(text)]


def reachable(edges: dict[str, set[str]], start: str) -> set[str]:
    seen, todo = set(), [start]
    while todo:
        for target in edges.get(todo.pop(), ()):
            if target not in seen:
                seen.add(target)
                todo.append(target)
    return seen


def cycles(edges: dict[str, set[str]]) -> list[set[str]]:
    """The sets of two components or more that each reach the others (strongly connected), sorted."""
    reach = {name: reachable(edges, name) for name in sorted(edges)}
    found: list[set[str]] = []
    for name in reach:
        group = {other for other in reach[name] if name in reach.get(other, ())} | {name}
        if len(group) > 1 and group not in found:
            found.append(group)
    return found


def shortest_path(edges: dict[str, set[str]], group: set[str], start: str, targets: set[str]) -> list[str]:
    """The shortest path inside the group from start to the first target reached, start excluded."""
    previous: dict[str, str | None] = {start: None}
    queue = [start]
    for current in queue:
        for target in sorted(edges.get(current, set()) & group):
            if target in targets:
                path = [target, current]
                while previous[path[-1]] is not None:
                    path.append(previous[path[-1]])
                return path[-2::-1]
            if target not in previous:
                previous[target] = current
                queue.append(target)
    raise ValueError(f"{sorted(targets)} not reachable from {start}")


def cycle_path(edges: dict[str, set[str]], group: set[str]) -> list[str]:
    """A closed path from the first component of the group through all the others and back."""
    start = min(group)
    path, left = [start], group - {start}
    while left:
        path += shortest_path(edges, group, path[-1], left)
        left -= set(path)
    return path + shortest_path(edges, group, path[-1], {start})


def cycle_problems(edges: dict[str, set[str]], admitted: Iterable[tuple[str, ...]], file: str) -> list[Problem]:
    """Each cycle not admitted, and each admitted cycle not found."""
    admitted = [set(names) for names in admitted]
    found = cycles(edges)
    problems = [Problem(Kind.CYCLE, " -> ".join(cycle_path(edges, group)), file)
                for group in found if group not in admitted]
    return problems + [Problem(Kind.STALE_CYCLE, " ".join(sorted(names)), file)
                       for names in admitted if names not in found]


def arrow_edges(arrows: Iterable[tuple[str, str]]) -> dict[str, set[str]]:
    edges: dict[str, set[str]] = {}
    for source, target in arrows:
        if source != target:
            edges.setdefault(source, set()).add(target)
    return edges


# .NET and Kotlin: a component is a namespace (package) right under the root, declared in the diagram.

def stereotype(component: str, root: str, components: Iterable[str]) -> str:
    """The .NET pattern of the full names (namespaces and types) of a component: itself and its sub-namespaces; for
    the root, itself and each name one segment below it that is not another component (its types)."""
    if component != root:
        return f"^{re.escape(component)}(\\..+)?$"
    children = sorted(re.escape(c[len(root) + 1:]) + "$" for c in components if c != root)
    excluded = f"(?!{'|'.join(children)})" if children else ""
    return f"^{re.escape(root)}(\\.{excluded}[^.]+)?$"


def kotlin_stereotype(component: str, root: str, components: Iterable[str]) -> str:
    """The ArchUnit package identifier of a component: the root package alone (its types), another component with
    its sub-packages."""
    return root if component == root else component + ".."


@dataclass(frozen=True)
class Language:
    extensions: tuple[str, ...]  # source files
    skipped: frozenset[str]  # build output folders
    declaration: re.Pattern  # the namespace or package line
    stereotype: Callable[[str, str, Iterable[str]], str]
    sources_required: bool  # the `' sources:` header line lists the folders scanned; else the whole repository
    undeclared: Kind
    outside_root: Kind
    no_declaration: Kind


DOTNET = Language((".cs",), frozenset(SKIPPED), NAMESPACE, stereotype, False,
                  Kind.UNDECLARED, Kind.OUTSIDE_ROOT, Kind.NO_NAMESPACE)
KOTLIN = Language((".kt", ".java"), frozenset(KOTLIN_SKIPPED), PACKAGE, kotlin_stereotype, True,
                  Kind.UNDECLARED_PACKAGE, Kind.OUTSIDE_ROOT_PACKAGE, Kind.NO_PACKAGE_LINE)


def component_of(namespace: str, root: str) -> str | None:
    if namespace == root:
        return root
    if namespace.startswith(root + "."):
        return root + "." + namespace[len(root) + 1:].split(".")[0]
    return None


def source_files(repo: Path, folders: Iterable[str], excluded: list[str], language: Language) -> list[str]:
    """The source files under the folders, outside hidden and build output folders and the excluded prefixes."""
    files = set()
    for folder in folders:
        base = repo / folder
        for extension in language.extensions:
            for path in base.rglob("*" + extension):
                if any(f.startswith(".") or f in language.skipped for f in path.relative_to(base).parts[:-1]):
                    continue
                relative = path.relative_to(repo).as_posix()
                if not any(relative.startswith(prefix) for prefix in excluded):
                    files.add(relative)
    return sorted(files)


@dataclass(frozen=True)
class PackageDiagram:
    text: str
    root: str  # root namespace or package
    files: tuple[str, ...]  # the source files checked


def read_package_diagram(repo: Path, language: Language) -> PackageDiagram | Problem:
    """The diagram's text, its root and the source files its header selects."""
    diagram = repo / DIAGRAM
    if not diagram.is_file():
        return Problem(Kind.NO_DIAGRAM, "", DIAGRAM)
    text = diagram.read_text(encoding="utf-8")
    header = {key: value.strip() for key, value in HEADER.findall(text)}
    root = header.get("root", "")
    if not root:
        return Problem(Kind.NO_ROOT, "", DIAGRAM)
    folders = header.get("sources", "").split() if language.sources_required else ["."]
    if not folders:
        return Problem(Kind.NO_SOURCES, "", DIAGRAM)
    excluded = [p.rstrip("/") + "/" for p in header.get("exclude", "").split()]
    return PackageDiagram(text, root, tuple(source_files(repo, folders, excluded, language)))


def scan_namespaces(repo: Path, diagram: PackageDiagram, declared: Iterable[str],
                    language: Language) -> tuple[dict[str, str], list[Problem]]:
    """The components of the code, each with the first file declaring it, and the problems in file order."""
    declared = set(declared)
    found: list[Problem] = []
    in_code: dict[str, str] = {}
    for relative in diagram.files:
        namespaces = language.declaration.findall((repo / relative).read_text(encoding="utf-8-sig"))
        if not namespaces:
            found.append(Problem(language.no_declaration, "", relative))
        for namespace in namespaces:
            component = component_of(namespace, diagram.root)
            if component is None:
                found.append(Problem(language.outside_root, namespace, relative))
            elif component not in in_code:
                in_code[component] = relative
                if component not in declared:
                    found.append(Problem(language.undeclared, component, relative))
    return in_code, found


def package_problems(repo: Path, language: Language) -> list[Problem]:
    diagram = read_package_diagram(repo, language)
    if isinstance(diagram, Problem):
        return [diagram]
    declared = dict(COMPONENT.findall(diagram.text))
    in_code, found = scan_namespaces(repo, diagram, declared, language)
    found += [Problem(Kind.ABSENT, c, DIAGRAM) for c in sorted(declared.keys() - in_code.keys())]
    found += [Problem(Kind.STALE_STEREOTYPE, c, DIAGRAM) for c, pattern in sorted(declared.items())
              if pattern != language.stereotype(c, diagram.root, declared)]
    arrows = [(arrow.group(1).strip(), arrow.group(2).strip())
              for arrow in map(ARROW.match, diagram.text.splitlines()) if arrow]
    return found + cycle_problems(arrow_edges(arrows), header_cycles(diagram.text), DIAGRAM)


def write_package_diagram(repo: Path, language: Language) -> list[Problem]:
    """Rewrite the components of the diagram from the namespaces (packages) of the code, with their stereotypes;
    drop the arrows to or from a component that is gone and keep the other lines. The architecture test writes the
    arrows."""
    diagram = read_package_diagram(repo, language)
    if isinstance(diagram, Problem):
        return [diagram]
    components = sorted(scan_namespaces(repo, diagram, (), language)[0])
    lines = diagram.text.splitlines()
    if "@startuml" not in (line.strip() for line in lines):
        return [Problem(Kind.NO_STARTUML, "", DIAGRAM)]
    body: list[str] = []
    for line in lines:
        arrow = ARROW.match(line)
        if COMPONENT.match(line) or (arrow and not set(arrow.groups()) <= set(components)):
            continue
        body.append(line)
        if line.strip() == "@startuml":
            body += [f"[{c}] <<{language.stereotype(c, diagram.root, components)}>>" for c in components]
    (repo / DIAGRAM).write_text("".join(line + "\n" for line in body), encoding="utf-8", newline="\n")
    return package_problems(repo, language)


def dotnet_problems(repo: Path) -> list[Problem]:
    return package_problems(repo, DOTNET)


def write_dotnet_diagram(repo: Path) -> list[Problem]:
    return write_package_diagram(repo, DOTNET)


def kotlin_problems(repo: Path) -> list[Problem]:
    return package_problems(repo, KOTLIN)


def write_kotlin_diagram(repo: Path) -> list[Problem]:
    return write_package_diagram(repo, KOTLIN)

# Python

def read_contracts(repo: Path) -> configparser.ConfigParser:
    config = configparser.ConfigParser()
    config.read(repo / IMPORTLINTER, encoding="utf-8")
    return config


def root_packages(config: configparser.ConfigParser) -> list[str]:
    """The root packages: `root_package`, else the lines of `root_packages`."""
    one = config.get("importlinter", "root_package", fallback="").strip()
    several = config.get("importlinter", "root_packages", fallback="").split()
    return [one] if one else several


def declared_components(config: configparser.ConfigParser) -> set[str]:
    return {config[s].get("component", "").strip() for s in config.sections()
            if s.startswith(CONTRACT_SECTION) and config[s].get("type", "").strip() == CONTRACT_TYPE}


def package_dir(repo: Path, root: str) -> Path | None:
    for base in (repo / "src", repo) if root else ():
        if (base / root / "__init__.py").is_file():
            return base / root
    return None


def children(repo: Path, package: Path, root: str) -> dict[str, Path]:
    """The components: the root package's __init__, then each sub-package (folder with __init__.py) and module."""
    found = {root: package / "__init__.py"}
    for entry in sorted(package.iterdir()):
        if entry.is_dir() and (entry / "__init__.py").is_file():
            found[f"{root}.{entry.name}"] = entry
        elif entry.is_file() and entry.suffix == ".py" and entry.name != "__init__.py":
            found[f"{root}.{entry.stem}"] = entry
    return found


def unchecked_folders(package: Path) -> list[Path]:
    """The topmost folders under the package that hold Python code but no __init__.py, which grimp skips.

    Only folders whose name can be imported: the others (`__pycache__`, `.venv`, `my-data`) hold no module."""
    found = []
    for entry in sorted(package.iterdir()):
        if not entry.is_dir() or not entry.name.isidentifier() or entry.name in PY_SKIPPED:
            continue
        if (entry / "__init__.py").is_file():
            found += unchecked_folders(entry)
        elif any(entry.rglob("*.py")):
            found.append(entry)
    return found


def python_problems(repo: Path) -> list[Problem]:
    if not (repo / IMPORTLINTER).is_file():
        return [Problem(Kind.NO_DIAGRAM, "", IMPORTLINTER)]
    config = read_contracts(repo)
    roots = root_packages(config)
    if not roots:
        return [Problem(Kind.NO_ROOT_PACKAGE, "", IMPORTLINTER)]
    packages = {root: package_dir(repo, root) for root in roots}
    missing = [Problem(Kind.NO_PACKAGE, root, IMPORTLINTER) for root, package in packages.items() if package is None]
    if missing:
        return missing
    declared = declared_components(config)
    in_code = components(repo, packages)
    found = [Problem(Kind.UNDECLARED_MODULE, name, path.relative_to(repo).as_posix())
             for name, path in in_code.items() if name not in declared]
    found += [Problem(Kind.NO_INIT, ".".join(folder.relative_to(package.parent).parts),
                      folder.relative_to(repo).as_posix())
              for package in packages.values() for folder in unchecked_folders(package)]
    found += [Problem(Kind.ABSENT, c, IMPORTLINTER) for c in sorted(declared - set(in_code))]
    allowed = {config[s].get("component", "").strip(): set(config[s].get("allowed", "").split())
               for s in config.sections()
               if s.startswith(CONTRACT_SECTION) and config[s].get("type", "").strip() == CONTRACT_TYPE}
    admitted = header_cycles((repo / IMPORTLINTER).read_text(encoding="utf-8"))
    return found + cycle_problems({name: imports - {name} for name, imports in allowed.items()}, admitted,
                                  IMPORTLINTER)


def components(repo: Path, packages: dict[str, Path]) -> dict[str, Path]:
    """The components of every root package, root by root."""
    return {name: path for root, package in packages.items() for name, path in children(repo, package, root).items()}


def module_component(module: str, roots: Iterable[str]) -> str | None:
    """The component holding a module of a root package (the root itself for its __init__); None for another package."""
    root, _, rest = module.partition(".")
    if root not in roots:
        return None
    return f"{root}.{rest.split('.')[0]}" if rest else root


def imported_modules(path: Path, module: str, is_package: bool) -> set[str]:
    """Every module a file imports, as grimp counts them: relative imports resolved, any depth in the tree."""
    found = set()
    for node in ast.walk(ast.parse(path.read_text(encoding="utf-8-sig"), str(path))):
        if isinstance(node, ast.Import):
            found.update(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom):
            if node.level:
                base = module.split(".") if is_package else module.split(".")[:-1]
                base = base[:len(base) - (node.level - 1)]
                prefix = ".".join(base + ([node.module] if node.module else []))
            else:
                prefix = node.module or ""
            found.update(f"{prefix}.{alias.name}" for alias in node.names)
    return found


def dependencies(repo: Path, roots: list[str]) -> dict[str, set[str]]:
    """For each component of the root packages, the other components it imports."""
    packages = {root: package_dir(repo, root) for root in roots}
    in_code = components(repo, packages)
    found: dict[str, set[str]] = {}
    for component, path in in_code.items():
        base = packages[component.split(".")[0]].parent
        files = sorted(path.rglob("*.py")) if path.is_dir() else [path]
        imported: set[str] = set()
        for file in files:
            parts = file.relative_to(base).with_suffix("").parts
            is_package = parts[-1] == "__init__"
            module = ".".join(parts[:-1] if is_package else parts)
            for m in imported_modules(file, module, is_package):
                if (c := module_component(m, roots)) is not None:
                    # `from shop import X`, X not a sub-module: an import of the root's __init__, as grimp counts it.
                    imported.add(c if c in in_code else c.split(".")[0])
        found[component] = imported - {component}
    return found


def ini_sections(lines: list[str]) -> tuple[str, list[tuple[str, list[str]]]]:
    """The header (text before the first section) and each section's name and lines, its [name] line included."""
    starts = [i for i, line in enumerate(lines) if line.startswith("[")]
    header = "".join(lines[:starts[0] if starts else len(lines)])
    bounds = zip(starts, starts[1:] + [len(lines)])
    return header, [(lines[s].strip()[1:].split("]")[0].strip(), lines[s:e]) for s, e in bounds]


def kept_options(body: list[str], dropped: set[str]) -> str:
    """The options of a section body, each with its continuation lines, except the dropped keys."""
    text, keep = "", True
    for line in body:
        if line.strip() and not line[0].isspace() and line[0] not in "#;":
            keep = re.split(r"[=:]", line, maxsplit=1)[0].strip().lower() not in dropped
        if keep and line.strip():
            text += line if line.endswith("\n") else line + "\n"
    return text


def contract_section(component: str, roots: list[str]) -> str:
    """The contract's section: the child's name (`__init__` for the root) with one root package, else the component."""
    if len(roots) > 1:
        return CONTRACT_SECTION + component
    return CONTRACT_SECTION + (component.partition(".")[2] or "__init__")


def contracts_text(header: str, roots: list[str], found: dict[str, set[str]], kept: str = "") -> str:
    """The .importlinter text: header, [importlinter], `kept` (other options and sections), the allowed contracts."""
    packages = (f"root_package = {roots[0]}\n" if len(roots) == 1
                else "root_packages =\n" + "".join(f"    {r}\n" for r in roots))
    text = (header + f"[importlinter]\n{packages}contract_types =\n"
            f"    {CONTRACT_TYPE}: architecture.AllowedContract\n" + kept)
    for component, allowed in found.items():
        text += (f"\n[{contract_section(component, roots)}]\nname = {component}\n"
                 f"type = {CONTRACT_TYPE}\ncomponent = {component}\n")
        if allowed:
            text += "allowed =\n" + "".join(f"    {a}\n" for a in sorted(allowed))
    return text


def write_contracts(repo: Path) -> list[Problem]:
    """Rewrite the allowed contracts of .importlinter from the code.

    Keeps the header comments, the other options of [importlinter] and its other contract
    types, and every other section (contracts of other types) as written."""
    found = [p for p in python_problems(repo) if p.kind in (Kind.NO_DIAGRAM, Kind.NO_ROOT_PACKAGE, Kind.NO_PACKAGE)]
    if found:
        return found
    file = repo / IMPORTLINTER
    config = read_contracts(repo)
    header, sections = ini_sections(file.read_text(encoding="utf-8").splitlines(keepends=True))
    types = [t.strip() for t in config.get("importlinter", "contract_types", fallback="").splitlines()]
    kept = "".join(f"    {t}\n" for t in types if t and t.split(":")[0].strip() != CONTRACT_TYPE)
    kept += "".join(kept_options(lines[1:], {"root_package", "root_packages", "contract_types"})
                    for name, lines in sections if name == "importlinter")
    for name, lines in sections:
        if name != "importlinter" and config[name].get("type", "").strip() != CONTRACT_TYPE:
            kept += "\n" + "".join(lines).rstrip() + "\n"
    roots = root_packages(config)
    file.write_text(contracts_text(header, roots, dependencies(repo, roots), kept), encoding="utf-8", newline="\n")
    return []


def illegal_imports(graph, component: str, allowed: set[str]) -> list[tuple[str, str, tuple[int, ...]]]:
    """The direct imports from a component to another one it does not allow, with their line numbers."""
    # The root packages: the graph's top-level modules, but the external packages import-linter squashes.
    roots = {m.split(".")[0] for m in graph.modules if not graph.is_module_squashed(m)}
    # A root component is the root package's __init__ alone: its descendants are the other components.
    importers = {component} if "." not in component else {component} | graph.find_descendants(component)
    found = []
    for importer in sorted(importers):
        for imported in sorted(graph.find_modules_directly_imported_by(importer)):
            target = module_component(imported, roots)
            if target and target != component and target not in allowed:
                details = graph.get_import_details(importer=importer, imported=imported)
                found.append((importer, imported, tuple(d["line_number"] for d in details)))
    return found


try:
    from importlinter import Contract, ContractCheck, fields, output
except ImportError:
    Contract = None

if Contract is not None:
    class AllowedContract(Contract):
        """A component (a child of the root package, or its __init__) imports directly only the components it lists."""

        component = fields.StringField()
        allowed = fields.SetField(subfield=fields.StringField(), required=False)

        def check(self, graph, verbose: bool) -> ContractCheck:
            found = illegal_imports(graph, str(self.component), set(self.allowed or ()))
            return ContractCheck(kept=not found, metadata={"imports": found})

        def render_broken_contract(self, check: ContractCheck) -> None:
            for importer, imported, lines in check.metadata["imports"]:
                output.print_error(f"- {importer} -> {imported} (l.{', '.join(map(str, lines))})", bold=False)
            output.new_line()
            output.print(f"{self.component} may import only: {', '.join(sorted(self.allowed or ())) or 'nothing'}. "
                         f"Allowing more changes {IMPORTLINTER}: an architecture change.")
            output.new_line()


def lint(repo: Path) -> bool:
    """Run import-linter on .importlinter, in this process so that it finds AllowedContract."""
    from importlinter.cli import lint_imports
    for base in {str(package_dir(repo, root).parent) for root in root_packages(read_contracts(repo))}:
        sys.path.insert(0, base)
    return lint_imports(config_filename=str(repo / IMPORTLINTER), no_cache=True, no_logo=True) == 0


# Rust

@dataclass(frozen=True)
class ToolFailure:
    message: str  # the tool's own error output, untranslated; empty when the tool is not installed


@dataclass(frozen=True)
class Crate:
    manifest: str  # Cargo.toml, relative to the workspace root
    crates: frozenset[str]  # workspace crates it depends on
    externals: tuple[str, ...]  # other crates it depends on, in manifest order


@dataclass(frozen=True)
class Diagram:
    header: str  # the lines before @startuml
    other: tuple[str, ...]  # lines of the body that are neither declarations nor arrows
    components: dict[str, str]  # name -> stereotype ("" when none)
    arrows: tuple[tuple[str, str], ...]


@dataclass(frozen=True)
class Workspace:
    text: str  # the diagram
    diagram: Diagram
    crates: dict[str, Crate]  # the components: workspace crates not excluded, by name


def cargo_metadata(repo: Path) -> dict | ToolFailure:
    try:
        result = subprocess.run(CARGO_METADATA, cwd=repo, capture_output=True, text=True, encoding="utf-8")
    except FileNotFoundError:
        return ToolFailure("")
    if result.returncode != 0:
        return ToolFailure(result.stderr.strip())
    return json.loads(result.stdout)


def workspace_crates(metadata: dict, excluded: set[str]) -> dict[str, Crate]:
    root = Path(metadata["workspace_root"])
    members = [p for p in metadata["packages"] if p["id"] in set(metadata["workspace_members"])]
    names = {p["name"] for p in members}
    found = {}
    for package in sorted(members, key=lambda p: p["name"]):
        if package["name"] in excluded:
            continue
        counted = [d["name"] for d in package["dependencies"] if d["kind"] in COUNTED_KINDS]
        found[package["name"]] = Crate(
            manifest=Path(package["manifest_path"]).relative_to(root).as_posix(),
            crates=frozenset(d for d in counted if d in names and d not in excluded and d != package["name"]),
            externals=tuple(dict.fromkeys(d for d in counted if d not in names)))
    return found


def read_diagram(text: str) -> Diagram:
    lines = text.splitlines()
    start = next((i for i, line in enumerate(lines) if line.strip().startswith("@startuml")), len(lines))
    other, components, arrows = [], {}, []
    for line in lines[start + 1:]:
        if line.strip().startswith("@enduml"):
            break
        if declaration := DECLARATION.match(line):
            components[declaration.group(1).strip()] = declaration.group(2) or ""
        elif arrow := ARROW.match(line):
            arrows.append((arrow.group(1).strip(), arrow.group(2).strip()))
        else:
            other.append(line)
    return Diagram("".join(line + "\n" for line in lines[:start]), tuple(other), components, tuple(arrows))


def read_workspace(repo: Path, metadata: dict | ToolFailure | None) -> Workspace | Problem:
    """The diagram and the crates of the workspace; metadata None runs cargo metadata in repo."""
    file = repo / DIAGRAM
    if not file.is_file():
        return Problem(Kind.NO_DIAGRAM, "", DIAGRAM)
    metadata = cargo_metadata(repo) if metadata is None else metadata
    if isinstance(metadata, ToolFailure):
        return Problem(Kind.CARGO_FAILED if metadata.message else Kind.NO_CARGO, metadata.message, "")
    text = file.read_text(encoding="utf-8")
    excluded = {name for key, value in HEADER.findall(text) if key == "exclude" for name in value.split()}
    return Workspace(text, read_diagram(text), workspace_crates(metadata, excluded))


def rust_problems(repo: Path, metadata: dict | ToolFailure | None = None) -> list[Problem]:
    workspace = read_workspace(repo, metadata)
    if isinstance(workspace, Problem):
        return [workspace]
    components, crates = workspace.diagram.components, workspace.crates
    declared = {name for name, stereotype in components.items() if stereotype != EXTERNAL}
    allowed = set(workspace.diagram.arrows)
    found = [Problem(Kind.UNDECLARED_CRATE, name, crate.manifest) for name, crate in crates.items()
             if name not in declared]
    found += [Problem(Kind.ABSENT, name, DIAGRAM) for name in sorted(declared - set(crates))]
    for name, crate in crates.items():
        found += [Problem(Kind.UNDRAWN_DEPENDENCY, f"{name} -> {dep}", crate.manifest)
                  for dep in sorted(crate.crates) if (name, dep) not in allowed]
        if components.get(name) == RESTRICTED:
            found += [Problem(Kind.UNDRAWN_EXTERNAL, f"{name} -> {dep}", crate.manifest)
                      for dep in crate.externals if (name, dep) not in allowed]
    found += [Problem(Kind.UNKNOWN_ARROW, f"{source} -> {target}", DIAGRAM)
              for source, target in workspace.diagram.arrows if source not in components or target not in components]
    return found + cycle_problems(arrow_edges(workspace.diagram.arrows), header_cycles(workspace.text), DIAGRAM)


def write_diagram(repo: Path, metadata: dict | ToolFailure | None = None) -> list[Problem]:
    """Rewrite the components and arrows of the diagram from the workspace, keeping its other lines."""
    workspace = read_workspace(repo, metadata)
    if isinstance(workspace, Problem):
        return [workspace]
    diagram, crates = workspace.diagram, workspace.crates
    restricted = [name for name in crates if diagram.components.get(name) == RESTRICTED]
    externals = sorted({dep for name in restricted for dep in crates[name].externals})
    arrows = sorted([(name, dep) for name, crate in crates.items() for dep in crate.crates]
                    + [(name, dep) for name in restricted for dep in crates[name].externals])
    body = (list(diagram.other)
            + [f"[{name}] <<{RESTRICTED}>>" if name in restricted else f"[{name}]" for name in crates]
            + [f"[{name}] <<{EXTERNAL}>>" for name in externals]
            + [f"[{source}] --> [{target}]" for source, target in arrows])
    (repo / DIAGRAM).write_text(diagram.header + "@startuml\n" + "".join(line + "\n" for line in body)
                                + "@enduml\n", encoding="utf-8", newline="\n")
    return []


# JS/TS

@dataclass(frozen=True)
class JsArchitecture:
    content: dict  # docs/architecture.json as read, written back by --write
    roots: tuple[str, ...]
    exclude: tuple[str, ...]  # folder prefixes, ending with /
    components: dict[str, tuple[str, ...]]  # component -> components it may import
    allowed_cycles: tuple[tuple[str, ...], ...]  # the cycles admitted, each by its components


def read_js(repo: Path) -> JsArchitecture | Problem:
    file = repo / ARCHITECTURE_JSON
    if not file.is_file():
        return Problem(Kind.NO_DIAGRAM, "", ARCHITECTURE_JSON)
    try:
        content = json.loads(file.read_text(encoding="utf-8"))
        roots = tuple(r.strip("/") or "." for r in content.get("roots", ()))
        exclude = tuple(e.rstrip("/") + "/" for e in content.get("exclude", ()))
        components = {name: tuple(allowed) for name, allowed in content.get("components", {}).items()}
        allowed_cycles = tuple(tuple(names) for names in content.get(ALLOWED_CYCLES, ()))
        if not all(isinstance(names, list) and all(isinstance(n, str) for n in names)
                   for names in content.get(ALLOWED_CYCLES, ())):
            raise TypeError(f"`{ALLOWED_CYCLES}`: a list of lists of component names expected")
    except (ValueError, AttributeError, TypeError) as error:
        return Problem(Kind.INVALID_FILE, str(error), ARCHITECTURE_JSON)
    if not roots:
        return Problem(Kind.NO_ROOTS, "", ARCHITECTURE_JSON)
    return JsArchitecture(content, roots, exclude, components, allowed_cycles)


def js_component(path: str, architecture: JsArchitecture) -> str | None:
    """The component of a source file (relative, with /); None outside the roots, excluded, or not a component."""
    parts = path.split("/")
    if (Path(path).suffix not in JS_SOURCES or JS_TESTS.search(path)
            or any(p.startswith(".") or p in JS_SKIPPED for p in parts)
            or any(path.startswith(prefix) for prefix in architecture.exclude)):
        return None
    for root in sorted(architecture.roots, key=len, reverse=True):
        prefix = "" if root == "." else root + "/"
        if path.startswith(prefix):
            rest = path[len(prefix):].split("/")
            return prefix + rest[0] if len(rest) > 1 else root
    return None


def js_sources(repo: Path, architecture: JsArchitecture) -> dict[str, str]:
    """Each source file of a component (relative, with /) -> its component."""
    found: dict[str, str] = {}
    for root in architecture.roots:
        for folder, dirs, files in os.walk(repo / root):
            dirs[:] = sorted(d for d in dirs if not d.startswith(".") and d not in JS_SKIPPED)
            for name in sorted(files):
                relative = (Path(folder) / name).relative_to(repo).as_posix()
                component = js_component(relative, architecture)
                if component is not None:
                    found[relative] = component
    return dict(sorted(found.items()))


def js_components_in_code(sources: dict[str, str]) -> dict[str, str]:
    """Each component of the code -> its first source file."""
    found: dict[str, str] = {}
    for path, component in sources.items():
        found.setdefault(component, path)
    return dict(sorted(found.items()))


@dataclass(frozen=True)
class Cruise:
    read: frozenset[str]  # the files dependency-cruiser read
    imports: tuple[tuple[str, str], ...]  # (importing file, imported file) it resolved


def depcruise(repo: Path, architecture: JsArchitecture) -> Cruise | ToolFailure:
    """What dependency-cruiser reads under the roots, paths relative to the repository."""
    # Not through npx: `npx depcruise` would fetch an unrelated package named depcruise when none is installed.
    command = shutil.which("depcruise", path=str(repo / "node_modules" / ".bin")) or shutil.which("depcruise")
    if command is None:
        return ToolFailure("")
    result = subprocess.run([command, *architecture.roots, "--config", DEPCRUISE_CONFIG, "--output-type", "json"],
                            cwd=repo, capture_output=True, text=True, encoding="utf-8")
    if result.returncode != 0:
        return ToolFailure((result.stderr or result.stdout).strip() or f"exit code {result.returncode}")
    modules = json.loads(result.stdout)["modules"]
    return Cruise(frozenset(m["source"] for m in modules),
                  tuple((m["source"], d["resolved"]) for m in modules for d in m.get("dependencies", ())))


def js_failure(failure: ToolFailure) -> Problem:
    return Problem(Kind.DEPCRUISE_FAILED if failure.message else Kind.NO_DEPCRUISE, failure.message, "")


def unread(sources: dict[str, str], cruise: Cruise) -> list[Problem]:
    """The first source file of each component that dependency-cruiser did not read (TypeScript without its
    compiler): its imports would pass unchecked."""
    found: dict[str, str] = {}
    for path, component in sources.items():
        if path not in cruise.read:
            found.setdefault(component, path)
    return [Problem(Kind.NOT_READ, "", path) for path in found.values()]


def component_imports(cruise: Cruise, architecture: JsArchitecture) -> list[tuple[str, str, str, str]]:
    """Each import between two different components: (importing file, imported file, their components)."""
    found = []
    for source, target in cruise.imports:
        source_component, target_component = js_component(source, architecture), js_component(target, architecture)
        if source_component and target_component and source_component != target_component:
            found.append((source, target, source_component, target_component))
    return found


def js_problems(repo: Path, cruise: Cruise | ToolFailure | None = None) -> list[Problem]:
    """cruise None runs dependency-cruiser in repo."""
    architecture = read_js(repo)
    if isinstance(architecture, Problem):
        return [architecture]
    declared = architecture.components
    sources = js_sources(repo, architecture)
    in_code = js_components_in_code(sources)
    found = [Problem(Kind.UNDECLARED_FOLDER, name, path) for name, path in in_code.items() if name not in declared]
    found += [Problem(Kind.ABSENT, name, ARCHITECTURE_JSON) for name in sorted(set(declared) - set(in_code))]
    found += [Problem(Kind.UNKNOWN_ALLOWED, f"{name} -> {other}", ARCHITECTURE_JSON)
              for name, allowed in declared.items() for other in allowed if other not in declared]
    found += cycle_problems({name: set(allowed) - {name} for name, allowed in declared.items()},
                            architecture.allowed_cycles, ARCHITECTURE_JSON)
    cruise = depcruise(repo, architecture) if cruise is None else cruise
    if isinstance(cruise, ToolFailure):
        return found + [js_failure(cruise)]
    found += unread(sources, cruise)
    found += [Problem(Kind.IMPORT_NOT_ALLOWED, f"{source_component} -> {target_component} ({target})", source)
              for source, target, source_component, target_component in component_imports(cruise, architecture)
              if source_component in declared and target_component in declared
              and target_component not in declared[source_component]]
    return found


def write_js(repo: Path, cruise: Cruise | ToolFailure | None = None) -> list[Problem]:
    """Rewrite `components` of docs/architecture.json from the code, keeping its other keys."""
    architecture = read_js(repo)
    if isinstance(architecture, Problem):
        return [architecture]
    cruise = depcruise(repo, architecture) if cruise is None else cruise
    if isinstance(cruise, ToolFailure):
        return [js_failure(cruise)]
    sources = js_sources(repo, architecture)
    found = unread(sources, cruise)
    if found:
        return found
    components: dict[str, set[str]] = {name: set() for name in js_components_in_code(sources)}
    for _, _, source_component, target_component in component_imports(cruise, architecture):
        components[source_component].add(target_component)
    content = {**architecture.content, "components": {name: sorted(allowed) for name, allowed in components.items()}}
    (repo / ARCHITECTURE_JSON).write_text(json.dumps(content, indent=2, ensure_ascii=False) + "\n", encoding="utf-8",
                                          newline="\n")
    return []


CHECKS = {"python": python_problems, "rust": rust_problems, "js": js_problems, "kotlin": kotlin_problems,
          "dotnet": dotnet_problems}
WRITERS = {"python": write_contracts, "rust": write_diagram, "js": write_js, "kotlin": write_kotlin_diagram,
           "dotnet": write_dotnet_diagram}


def problems(repo: Path) -> list[Problem]:
    return CHECKS[stack(repo)](repo)


def main() -> None:
    args = [a for a in sys.argv[1:] if a != "--write"]
    repo = Path(args[0]) if args else Path.cwd()
    name = stack(repo)
    python = name == "python"
    if "--write" in sys.argv[1:]:
        found = WRITERS[name](repo)
        for problem in found:
            print(problem)
        sys.exit(1 if found else 0)
    found = problems(repo)
    if python and Contract is None:
        found.append(Problem(Kind.NO_IMPORT_LINTER, "", ""))
    for problem in found:
        print(problem)
    passed = not found
    roots = root_packages(read_contracts(repo)) if python else []
    if roots and Contract is not None and all(package_dir(repo, root) for root in roots):
        passed = lint(repo) and passed
    sys.exit(0 if passed else 1)


if __name__ == "__main__":
    main()
