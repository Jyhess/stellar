#!/usr/bin/env python3
"""Check that every component of the code is declared in the architecture file (dev-kit #57, #74, #76).

    python3 .github/scripts/architecture.py [repository dir]              # .NET
    python3 .github/scripts/architecture.py [--write] [repository dir]    # Rust
    <runner> python .github/scripts/architecture.py [--write] [repository dir]   # Python

The stack is read from the `stack:` header line of its architecture file (STACK_FILES);
a diagram without one is .NET.

.NET: a component is a namespace right under the root namespace, sub-namespaces included
(`Shop.Orders` holds `Shop.Orders.Lines`); types declared in the root namespace itself
form the root component. The diagram header gives the root and the folders to skip:

    ' root: Shop
    ' exclude: tests/ tools/

The diagram declares a component as `[Shop.Orders] <<^Shop\\.Orders(\\..+)?$>>` (see
`stereotype`); ArchitectureTest.cs checks its arrows with ArchUnitNET, which ignores
the types of undeclared namespaces, hence this check. It fails when a namespace of the
code is not declared, a declared component has no code, a namespace is outside the
root, or a `.cs` file has no namespace. Hidden folders, `bin/` and `obj/` are skipped.

Python: a component is a direct child of the root package (`src/<root>/` or `<root>/`),
a sub-package with its sub-modules or a module. `.importlinter` declares each one as an
`allowed` contract (AllowedContract, registered through `contract_types`) that lists the
components it may import directly; the root package's `__init__` is not a component and
importing it is not checked (TODO #93), nor are folders without __init__.py (TODO #94).
The check fails when a child is not declared, a declared component has no code, or a
component imports one it does not allow (import-linter's
report, run in-process: import-linter must be installed in the project environment).
`--write` rewrites the contracts from the imports found today by `ast`, counted as grimp
counts them (relative, inside functions, under TYPE_CHECKING); it keeps only
`root_package` and `contract_types` in [importlinter] (TODO #95).

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
"""

from __future__ import annotations

import ast
import configparser
import enum
import json
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

DIAGRAM = "docs/architecture.puml"
IMPORTLINTER = ".importlinter"
STACK_FILES = {"python": IMPORTLINTER, "rust": DIAGRAM, "dotnet": DIAGRAM}
STACK = re.compile(r"(?m)^\s*[;'#]\s*stack:\s*(\S+)")
SKIPPED = {"bin", "obj"}
NAMESPACE = re.compile(r"(?m)^\s*namespace\s+([A-Za-z_][\w.]*)")
COMPONENT = re.compile(r"(?m)^\s*\[([^\]]+)\]\s*<<")
HEADER = re.compile(r"(?m)^'\s*(root|exclude):(.*)$")
CONTRACT_SECTION = "importlinter:contract:"
CONTRACT_TYPE = "allowed"
RESTRICTED = "restricted"
EXTERNAL = "external"
DECLARATION = re.compile(r"^\s*\[([^\]]+)\]\s*(?:<<\s*(\w+)\s*>>)?\s*$")
ARROW = re.compile(r"^\s*\[([^\]]+)\]\s*[-.]+(?:\w+[-.]+)?>\s*\[([^\]]+)\]\s*$")
COUNTED_KINDS = {None, "build"}  # dependency kinds of cargo metadata that count: normal and build, not dev
CARGO_METADATA = ["cargo", "metadata", "--no-deps", "--format-version", "1"]


class Kind(enum.Enum):
    UNDECLARED = "namespace not declared in " + DIAGRAM
    ABSENT = "component declared but no code in it"
    OUTSIDE_ROOT = "namespace outside the root namespace"
    NO_NAMESPACE = "file without namespace"
    NO_DIAGRAM = "diagram missing"
    NO_ROOT = "no `' root: <namespace>` line in the diagram header"
    UNDECLARED_MODULE = "component not declared in " + IMPORTLINTER
    NO_ROOT_PACKAGE = "no `root_package` in the [importlinter] section"
    NO_PACKAGE = "root package not found (src/<root>/__init__.py or <root>/__init__.py)"
    NO_IMPORT_LINTER = "import-linter is not installed in this environment"
    UNDECLARED_CRATE = "crate not declared in " + DIAGRAM
    UNDRAWN_DEPENDENCY = "dependency not drawn in " + DIAGRAM
    UNDRAWN_EXTERNAL = "external dependency of a <<restricted>> crate not drawn in " + DIAGRAM
    UNKNOWN_ARROW = "arrow to or from a component not declared"
    NO_CARGO = "cargo not found"
    CARGO_FAILED = "cargo metadata failed"
    NO_WRITE = "--write is for Python and Rust; .NET: ARCHITECTURE_DIAGRAM=write dotnet test"


@dataclass(frozen=True)
class Problem:
    kind: Kind
    name: str
    path: str

    def __str__(self) -> str:
        return ": ".join(part for part in (self.path, self.name, self.kind.value) if part)


def stack(repo: Path) -> str:
    """The stack whose architecture file names it in its header; .NET for a diagram without one."""
    for name, path in STACK_FILES.items():
        file = repo / path
        if file.is_file() and name in STACK.findall(file.read_text(encoding="utf-8")):
            return name
    return "dotnet"


# .NET

def stereotype(component: str, root: bool = False) -> str:
    """The namespace pattern of a component: itself and its sub-namespaces, or only itself for the root."""
    escaped = re.escape(component)
    return f"^{escaped}$" if root else f"^{escaped}(\\..+)?$"


def component_of(namespace: str, root: str) -> str | None:
    if namespace == root:
        return root
    if namespace.startswith(root + "."):
        return root + "." + namespace[len(root) + 1:].split(".")[0]
    return None


def source_files(repo: Path, excluded: list[str]) -> list[str]:
    files = []
    for path in repo.rglob("*.cs"):
        relative = path.relative_to(repo).as_posix()
        folders = relative.split("/")[:-1]
        if any(f.startswith(".") or f in SKIPPED for f in folders):
            continue
        if any(relative.startswith(prefix) for prefix in excluded):
            continue
        files.append(relative)
    return sorted(files)


def dotnet_problems(repo: Path) -> list[Problem]:
    diagram = repo / DIAGRAM
    if not diagram.is_file():
        return [Problem(Kind.NO_DIAGRAM, "", DIAGRAM)]
    text = diagram.read_text(encoding="utf-8")
    header = {key: value.strip() for key, value in HEADER.findall(text)}
    root = header.get("root", "")
    if not root:
        return [Problem(Kind.NO_ROOT, "", DIAGRAM)]
    excluded = [p.rstrip("/") + "/" for p in header.get("exclude", "").split()]
    declared = set(COMPONENT.findall(text))
    found: list[Problem] = []
    in_code: set[str] = set()
    for relative in source_files(repo, excluded):
        namespaces = NAMESPACE.findall((repo / relative).read_text(encoding="utf-8-sig"))
        if not namespaces:
            found.append(Problem(Kind.NO_NAMESPACE, "", relative))
        for namespace in namespaces:
            component = component_of(namespace, root)
            if component is None:
                found.append(Problem(Kind.OUTSIDE_ROOT, namespace, relative))
            elif component not in in_code:
                in_code.add(component)
                if component not in declared:
                    found.append(Problem(Kind.UNDECLARED, component, relative))
    found += [Problem(Kind.ABSENT, c, DIAGRAM) for c in sorted(declared - in_code)]
    return found


# Python

def read_contracts(repo: Path) -> configparser.ConfigParser:
    config = configparser.ConfigParser()
    config.read(repo / IMPORTLINTER, encoding="utf-8")
    return config


def root_package(config: configparser.ConfigParser) -> str:
    return config.get("importlinter", "root_package", fallback="").strip()


def declared_components(config: configparser.ConfigParser) -> set[str]:
    return {config[s].get("component", "").strip() for s in config.sections()
            if s.startswith(CONTRACT_SECTION) and config[s].get("type", "").strip() == CONTRACT_TYPE}


def package_dir(repo: Path, root: str) -> Path | None:
    for base in (repo / "src", repo) if root else ():
        if (base / root / "__init__.py").is_file():
            return base / root
    return None


def children(repo: Path, package: Path, root: str) -> dict[str, Path]:
    """The components: each sub-package (folder with __init__.py) and module of the root package."""
    found = {}
    for entry in sorted(package.iterdir()):
        if entry.is_dir() and (entry / "__init__.py").is_file():
            found[f"{root}.{entry.name}"] = entry
        elif entry.is_file() and entry.suffix == ".py" and entry.name != "__init__.py":
            found[f"{root}.{entry.stem}"] = entry
    return found


def python_problems(repo: Path) -> list[Problem]:
    if not (repo / IMPORTLINTER).is_file():
        return [Problem(Kind.NO_DIAGRAM, "", IMPORTLINTER)]
    config = read_contracts(repo)
    root = root_package(config)
    if not root:
        return [Problem(Kind.NO_ROOT_PACKAGE, "", IMPORTLINTER)]
    package = package_dir(repo, root)
    if package is None:
        return [Problem(Kind.NO_PACKAGE, root, IMPORTLINTER)]
    declared = declared_components(config)
    in_code = children(repo, package, root)
    found = [Problem(Kind.UNDECLARED_MODULE, name, path.relative_to(repo).as_posix())
             for name, path in in_code.items() if name not in declared]
    return found + [Problem(Kind.ABSENT, c, IMPORTLINTER) for c in sorted(declared - set(in_code))]


def module_component(module: str, root: str) -> str | None:
    """The component holding a module of the root package; None for the root itself or another package."""
    if not module.startswith(root + "."):
        return None
    return root + "." + module[len(root) + 1:].split(".")[0]


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


def dependencies(repo: Path) -> tuple[str, dict[str, set[str]]]:
    """The root package and, for each component, the other components it imports."""
    root = root_package(read_contracts(repo))
    package = package_dir(repo, root)
    components = children(repo, package, root)
    found: dict[str, set[str]] = {}
    for component, path in components.items():
        files = sorted(path.rglob("*.py")) if path.is_dir() else [path]
        imported: set[str] = set()
        for file in files:
            parts = file.relative_to(package.parent).with_suffix("").parts
            is_package = parts[-1] == "__init__"
            module = ".".join(parts[:-1] if is_package else parts)
            imported |= {c for m in imported_modules(file, module, is_package)
                         if (c := module_component(m, root)) in components}
        found[component] = imported - {component}
    return root, found


def contracts_text(header: str, root: str, found: dict[str, set[str]]) -> str:
    text = header + f"[importlinter]\nroot_package = {root}\ncontract_types =\n    {CONTRACT_TYPE}: architecture.AllowedContract\n"
    for component, allowed in found.items():
        text += (f"\n[{CONTRACT_SECTION}{component[len(root) + 1:]}]\nname = {component}\n"
                 f"type = {CONTRACT_TYPE}\ncomponent = {component}\n")
        if allowed:
            text += "allowed =\n" + "".join(f"    {a}\n" for a in sorted(allowed))
    return text


def write_contracts(repo: Path) -> list[Problem]:
    """Rewrite the contracts of .importlinter from the code, keeping its header comments."""
    found = [p for p in python_problems(repo) if p.kind in (Kind.NO_DIAGRAM, Kind.NO_ROOT_PACKAGE, Kind.NO_PACKAGE)]
    if found:
        return found
    file = repo / IMPORTLINTER
    lines = file.read_text(encoding="utf-8").splitlines(keepends=True)
    header = "".join(line for line in lines[:next((i for i, l in enumerate(lines) if l.startswith("[")), len(lines))])
    root, imports = dependencies(repo)
    file.write_text(contracts_text(header, root, imports), encoding="utf-8", newline="\n")
    return []


def illegal_imports(graph, component: str, allowed: set[str]) -> list[tuple[str, str, tuple[int, ...]]]:
    """The direct imports from a component to another one it does not allow, with their line numbers."""
    root = component.rsplit(".", 1)[0]
    found = []
    for importer in sorted({component} | graph.find_descendants(component)):
        for imported in sorted(graph.find_modules_directly_imported_by(importer)):
            target = module_component(imported, root)
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
        """A component (a child of the root package) imports directly only the components it lists."""

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
    package = package_dir(repo, root_package(read_contracts(repo)))
    sys.path.insert(0, str(package.parent))
    return lint_imports(config_filename=str(repo / IMPORTLINTER), no_cache=True, no_logo=True) == 0


# Rust

@dataclass(frozen=True)
class CargoFailure:
    message: str  # cargo's own error output, untranslated; empty when cargo is not installed


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


def cargo_metadata(repo: Path) -> dict | CargoFailure:
    try:
        result = subprocess.run(CARGO_METADATA, cwd=repo, capture_output=True, text=True, encoding="utf-8")
    except FileNotFoundError:
        return CargoFailure("")
    if result.returncode != 0:
        return CargoFailure(result.stderr.strip())
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


def read_workspace(repo: Path, metadata: dict | CargoFailure | None) -> Workspace | Problem:
    """The diagram and the crates of the workspace; metadata None runs cargo metadata in repo."""
    file = repo / DIAGRAM
    if not file.is_file():
        return Problem(Kind.NO_DIAGRAM, "", DIAGRAM)
    metadata = cargo_metadata(repo) if metadata is None else metadata
    if isinstance(metadata, CargoFailure):
        return Problem(Kind.CARGO_FAILED if metadata.message else Kind.NO_CARGO, metadata.message, "")
    text = file.read_text(encoding="utf-8")
    excluded = {name for key, value in HEADER.findall(text) if key == "exclude" for name in value.split()}
    return Workspace(text, read_diagram(text), workspace_crates(metadata, excluded))


def rust_problems(repo: Path, metadata: dict | CargoFailure | None = None) -> list[Problem]:
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
    return found


def write_diagram(repo: Path, metadata: dict | CargoFailure | None = None) -> list[Problem]:
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


CHECKS = {"python": python_problems, "rust": rust_problems, "dotnet": dotnet_problems}
WRITERS = {"python": write_contracts, "rust": write_diagram}


def problems(repo: Path) -> list[Problem]:
    return CHECKS[stack(repo)](repo)


def main() -> None:
    args = [a for a in sys.argv[1:] if a != "--write"]
    repo = Path(args[0]) if args else Path.cwd()
    name = stack(repo)
    python = name == "python"
    if "--write" in sys.argv[1:]:
        found = WRITERS[name](repo) if name in WRITERS else [Problem(Kind.NO_WRITE, "", "")]
        for problem in found:
            print(problem)
        sys.exit(1 if found else 0)
    found = problems(repo)
    if python and Contract is None:
        found.append(Problem(Kind.NO_IMPORT_LINTER, "", ""))
    for problem in found:
        print(problem)
    passed = not found
    if python and Contract is not None and package_dir(repo, root_package(read_contracts(repo))):
        passed = lint(repo) and passed
    sys.exit(0 if passed else 1)


if __name__ == "__main__":
    main()
