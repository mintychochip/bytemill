//! `.bml` loading, porting `bps/load.py`.
//!
//! An import is a path relative to the file that contains it, and each file is
//! loaded once. A file sees its own declarations and those of the files it
//! imports *directly*; imports are not transitive (the `.proto` rule).

use crate::ast::Schema;
use crate::errors::{BmlError, BmlResult, parse, py_repr_str, resolve};
use crate::parser::parse as parse_text;
use crate::primitives::is_bare_primitive;
use crate::refs::{Ref, RefKind, references};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

struct Module {
    path: PathBuf,
    text: String,
    schema: Schema,
    imports: Vec<PathBuf>,
}

/// One undeclared name, with the declaration that uses it and any import to add.
#[derive(Debug, Clone)]
pub(crate) struct UnresolvedUse {
    pub name: String,
    pub where_: String,
    pub kind: RefKind,
    pub imports: Vec<String>,
}

/// A load failure. `error` is the byte-identical library error `load_schema` returns.
#[derive(Debug)]
pub(crate) struct LoadFault {
    pub shown: String,
    pub text: Option<String>,
    pub error: BmlError,
    pub uses: Vec<UnresolvedUse>,
    pub visible_types: Vec<String>,
    pub visible_packets: Vec<String>,
}

impl LoadFault {
    fn bare(path: &Path, error: BmlError) -> Self {
        LoadFault {
            shown: show(path),
            text: None,
            error,
            uses: Vec::new(),
            visible_types: Vec::new(),
            visible_packets: Vec::new(),
        }
    }

    fn at(path: &Path, error: BmlError, text: String) -> Self {
        LoadFault {
            shown: show(path),
            text: Some(text),
            error,
            uses: Vec::new(),
            visible_types: Vec::new(),
            visible_packets: Vec::new(),
        }
    }
}

/// One loaded file, in import-before-importer order.
#[derive(Debug, Clone)]
pub(crate) struct LoadedFile {
    pub shown: String,
    pub text: String,
}

/// A schema that loaded, plus the files it was built from.
#[derive(Debug)]
pub(crate) struct Loaded {
    pub schema: Schema,
    pub files: Vec<LoadedFile>,
}

pub fn load_schema(paths: &[impl AsRef<Path>]) -> BmlResult<Schema> {
    load_for_diag(paths)
        .map(|loaded| loaded.schema)
        .map_err(|fault| fault.error)
}

/// [`load_schema`], plus the per-file text and structured unresolved names the
/// diagnostics layer renders. The `Err` payload's `error` is what [`load_schema`] returns.
pub(crate) fn load_for_diag(paths: &[impl AsRef<Path>]) -> Result<Loaded, Box<LoadFault>> {
    let modules = load_modules(paths)?;
    let providers = Providers::new(modules.values());
    for module in modules.values() {
        check_imports(module, &modules, &providers)?;
    }
    let files = modules
        .values()
        .map(|m| LoadedFile {
            shown: show(&m.path),
            text: m.text.clone(),
        })
        .collect();
    Ok(Loaded {
        schema: merge(modules.into_values()),
        files,
    })
}

/// Every file reachable from `paths`, each imported file before its importer.
struct ModuleTable {
    order: Vec<Module>,
    index: HashMap<PathBuf, usize>,
}

impl ModuleTable {
    fn new() -> Self {
        ModuleTable {
            order: Vec::new(),
            index: HashMap::new(),
        }
    }

    fn contains(&self, path: &Path) -> bool {
        self.index.contains_key(path)
    }

    fn values(&self) -> impl Iterator<Item = &Module> {
        self.order.iter()
    }

    fn into_values(self) -> impl Iterator<Item = Module> {
        self.order.into_iter()
    }
}

fn load_modules(paths: &[impl AsRef<Path>]) -> Result<ModuleTable, Box<LoadFault>> {
    let mut modules = ModuleTable::new();
    let mut loading: Vec<PathBuf> = Vec::new();

    fn add(
        raw: &Path,
        importer: Option<&Path>,
        modules: &mut ModuleTable,
        loading: &mut Vec<PathBuf>,
    ) -> Result<(), Box<LoadFault>> {
        let path = resolve_path(raw);
        if modules.contains(&path) || loading.contains(&path) {
            return Ok(());
        }
        if !path.is_file() {
            let by = match importer {
                Some(p) => format!(" (imported by {})", show(p)),
                None => String::new(),
            };
            return Err(Box::new(LoadFault::bare(
                &path,
                parse(format!("schema not found: {}{by}", show(&path))),
            )));
        }
        loading.push(path.clone());
        let text = match crate::text::read_text(&path) {
            Ok(text) => text,
            Err(e) => {
                return Err(Box::new(LoadFault::bare(
                    &path,
                    parse(format!("cannot read {}: {e}", show(&path))),
                )));
            }
        };
        let schema = match parse_text(&text) {
            Ok(schema) => schema,
            Err(e) => return Err(Box::new(LoadFault::at(&path, e, text))),
        };
        let imports: Vec<PathBuf> = schema
            .imports
            .iter()
            .map(|imp| resolve_path(&path.parent().unwrap_or(Path::new(".")).join(imp)))
            .collect();
        for imported in &imports {
            add(imported, Some(&path), modules, loading)?;
        }
        loading.pop();
        modules.index.insert(path.clone(), modules.order.len());
        modules.order.push(Module {
            path,
            text,
            schema,
            imports,
        });
        Ok(())
    }

    for raw in paths {
        add(raw.as_ref(), None, &mut modules, &mut loading)?;
    }
    Ok(modules)
}

/// Python `Path.resolve()`: absolute, symlinks followed when possible.
fn resolve_path(path: &Path) -> PathBuf {
    match std::fs::canonicalize(path) {
        Ok(p) => p,
        Err(_) => {
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(path))
                    .unwrap_or_else(|_| path.to_path_buf())
            }
        }
    }
}

/// Which files declare each name.
struct Providers {
    types_: HashMap<String, Vec<PathBuf>>,
    packets: HashMap<String, Vec<PathBuf>>,
}

impl Providers {
    fn new<'a>(modules: impl Iterator<Item = &'a Module>) -> Self {
        let mut types_ = HashMap::new();
        let mut packets = HashMap::new();
        for module in modules {
            for enc in &module.schema.encodings {
                types_
                    .entry(enc.name.clone())
                    .or_insert_with(Vec::new)
                    .push(module.path.clone());
            }
            for e in &module.schema.enums {
                types_
                    .entry(e.name.clone())
                    .or_insert_with(Vec::new)
                    .push(module.path.clone());
            }
            for pkt in &module.schema.packets {
                packets
                    .entry(pkt.name.clone())
                    .or_insert_with(Vec::new)
                    .push(module.path.clone());
            }
        }
        Providers { types_, packets }
    }

    fn of(&self, ref_: &Ref) -> &[PathBuf] {
        let table = if ref_.kind == RefKind::Packet {
            &self.packets
        } else {
            &self.types_
        };
        table.get(&ref_.name).map(|v| v.as_slice()).unwrap_or(&[])
    }
}

fn check_imports(
    module: &Module,
    modules: &ModuleTable,
    providers: &Providers,
) -> Result<(), Box<LoadFault>> {
    let mut visible: Vec<PathBuf> = vec![module.path.clone()];
    visible.extend(module.imports.iter().cloned());
    // file to import -> names it provides (insertion-ordered like Python's dict)
    let mut missing: Vec<(PathBuf, Vec<String>)> = Vec::new();
    let mut nowhere: Vec<(String, Ref)> = Vec::new(); // per (name, where_) for diags; error text will dedup names

    for ref_ in references(&module.schema) {
        if ref_.kind != RefKind::Packet && is_bare_primitive(&ref_.name) {
            continue;
        }
        let declared_in = providers.of(&ref_);
        if visible.iter().any(|v| declared_in.contains(v)) {
            continue;
        }
        if declared_in.is_empty()
            && !nowhere
                .iter()
                .any(|(n, r)| n == &ref_.name && r.where_ == ref_.where_)
        {
            nowhere.push((ref_.name.clone(), ref_.clone()));
        }
        for provider in declared_in {
            if let Some(entry) = missing.iter_mut().find(|(p, _)| p == provider) {
                if !entry.1.contains(&ref_.name) {
                    entry.1.push(ref_.name.clone());
                }
            } else {
                missing.push((provider.clone(), vec![ref_.name.clone()]));
            }
        }
    }
    if !nowhere.is_empty() {
        let nearby = nearby_providers(module, modules);
        let mut kept: Vec<(String, Ref)> = Vec::new();
        for (name, ref_) in nowhere {
            // Probe with the reference itself: a packet is looked up among packets, a type among types.
            let found = nearby.of(&ref_);
            if found.is_empty() {
                kept.push((name, ref_));
                continue;
            }
            for provider in found {
                if let Some(entry) = missing.iter_mut().find(|(p, _)| p == provider) {
                    if !entry.1.contains(&name) {
                        entry.1.push(name.clone());
                    }
                } else {
                    missing.push((provider.clone(), vec![name.clone()]));
                }
            }
        }
        nowhere = kept;
    }
    if missing.is_empty() && nowhere.is_empty() {
        return Ok(());
    }

    let mut text_nowhere: Vec<(String, Ref)> = Vec::new();
    for (name, r) in &nowhere {
        if !text_nowhere.iter().any(|(n, _)| n == name) {
            text_nowhere.push((name.clone(), r.clone()));
        }
    }

    let mut names: Vec<String> = Vec::new();
    for (_, ns) in &missing {
        for n in ns {
            if !names.contains(n) {
                names.push(n.clone());
            }
        }
    }
    let count = names.len() + text_nowhere.len();
    let mut lines = vec![format!(
        "{}: {} name{} not declared here or in an imported file",
        show(&module.path),
        count,
        if count == 1 { "" } else { "s" }
    )];
    for (provider, names) in &missing {
        let target = rel_path(provider, &module.path);
        lines.push(format!(
            "  add  import \"{target}\"  for {}",
            names.join(", ")
        ));
    }
    for (name, ref_) in &text_nowhere {
        lines.push(format!(
            "  {} (used in {}) is declared in no loaded or nearby .bml file",
            py_repr_str(name),
            ref_.where_
        ));
    }
    let (visible_types, visible_packets) = visible_names(module, modules);
    Err(Box::new(LoadFault {
        shown: show(&module.path),
        text: Some(module.text.clone()),
        error: resolve(lines.join("\n")),
        uses: build_uses(module, &missing, &nowhere),
        visible_types,
        visible_packets,
    }))
}

fn build_uses(
    module: &Module,
    missing: &[(PathBuf, Vec<String>)],
    nowhere: &[(String, Ref)],
) -> Vec<UnresolvedUse> {
    let refs = references(&module.schema);
    let mut uses: Vec<UnresolvedUse> = Vec::new();
    for (provider, names) in missing {
        let target = rel_path(provider, &module.path);
        for name in names {
            if let Some(existing) = uses.iter_mut().find(|u| u.name == *name) {
                existing.imports.push(target.clone());
                continue;
            }
            let site = refs.iter().find(|r| r.name == *name);
            uses.push(UnresolvedUse {
                name: name.clone(),
                where_: site.map(|r| r.where_.clone()).unwrap_or_default(),
                kind: site.map(|r| r.kind).unwrap_or(RefKind::Type),
                imports: vec![target.clone()],
            });
        }
    }
    for (name, ref_) in nowhere {
        uses.push(UnresolvedUse {
            name: name.clone(),
            where_: ref_.where_.clone(),
            kind: ref_.kind,
            imports: Vec::new(),
        });
    }
    uses
}

fn visible_names(module: &Module, modules: &ModuleTable) -> (Vec<String>, Vec<String>) {
    let mut types_ = Vec::new();
    let mut packets = Vec::new();
    let mut consider = |m: &Module| {
        for enc in &m.schema.encodings {
            types_.push(enc.name.clone());
        }
        for e in &m.schema.enums {
            types_.push(e.name.clone());
        }
        for pkt in &m.schema.packets {
            packets.push(pkt.name.clone());
        }
    };
    consider(module);
    for imp in &module.imports {
        if let Some(&i) = modules.index.get(imp) {
            consider(&modules.order[i]);
        }
    }
    (types_, packets)
}

/// The `.bml` files `dir.glob("*.bml")` (`depth` 0) or `dir.glob("*/*.bml")` (`depth` 1) finds.
/// Like pathlib's glob, not `glob.glob`, hidden names match.
fn bml_files(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let folders: Vec<PathBuf> = if depth == 0 {
        vec![dir.to_path_buf()]
    } else {
        match std::fs::read_dir(dir) {
            Ok(entries) => entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect(),
            Err(_) => Vec::new(),
        }
    };
    let mut out = Vec::new();
    for folder in folders {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().ends_with(".bml") && entry.path().is_file() {
                out.push(entry.path());
            }
        }
    }
    out
}

/// Providers among the unloaded `.bml` files beside `module` and one folder
/// down. This only words an error: a name a nearby file declares gets the
/// `import` line to add, not a dead end. A file that cannot be read is skipped
/// so it cannot hide the error being reported.
fn nearby_providers(module: &Module, loaded: &ModuleTable) -> Providers {
    let dir = module.path.parent().unwrap_or(Path::new("."));
    let mut found: Vec<Module> = Vec::new();
    for depth in [0, 1] {
        let mut paths = bml_files(dir, depth);
        paths.sort();
        for path in paths {
            let resolved = resolve_path(&path);
            if loaded.contains(&resolved) {
                continue;
            }
            let Ok(text) = crate::text::read_text(&resolved) else {
                continue;
            };
            let Ok(schema) = parse_text(&text) else {
                continue;
            };
            found.push(Module {
                path: resolved,
                text,
                schema,
                imports: Vec::new(),
            });
        }
    }
    Providers::new(found.iter())
}

fn merge(modules: impl Iterator<Item = Module>) -> Schema {
    let mut schema = Schema::default();
    for module in modules {
        schema.encodings.extend(module.schema.encodings);
        schema.enums.extend(module.schema.enums);
        schema.packets.extend(module.schema.packets);
        schema.protocols.extend(module.schema.protocols);
    }
    schema
}

/// `Path._show`: relative to the cwd when possible, else the absolute posix path.
fn show(path: &Path) -> String {
    if let Ok(cwd) = std::env::current_dir()
        && let Ok(rel) = path.strip_prefix(&cwd)
    {
        return posix(rel);
    }
    posix(path)
}

fn rel_path(target: &Path, base: &Path) -> String {
    let base_dir = base.parent().unwrap_or(Path::new("."));
    let mut rel = PathBuf::new();
    let mut t: Vec<std::ffi::OsString> = target.iter().map(|c| c.to_os_string()).collect();
    let mut b: Vec<std::ffi::OsString> = base_dir.iter().map(|c| c.to_os_string()).collect();
    // Drop the shared prefix, then emit one ".." per remaining base segment.
    while !t.is_empty() && !b.is_empty() && t[0] == b[0] {
        t.remove(0);
        b.remove(0);
    }
    for _ in b {
        rel.push("..");
    }
    for comp in t {
        rel.push(comp);
    }
    posix(&rel)
}

/// `Path.as_posix()`: components joined by `/`, with the root contributing its one slash.
fn posix(path: &Path) -> String {
    use std::path::Component;
    let mut out = String::new();
    for component in path.components() {
        match component {
            Component::RootDir => out.push('/'),
            other => {
                if !out.is_empty() && !out.ends_with('/') {
                    out.push('/');
                }
                out.push_str(&other.as_os_str().to_string_lossy());
            }
        }
    }
    out
}
