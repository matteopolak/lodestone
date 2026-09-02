//! The `/function`/`/reload` datapack function library (issue #48's
//! remainder) — vanilla's `ServerFunctionLibrary`/`TagLoader<CommandFunction>`,
//! restated as a plain directory scan behind one shared, cloneable handle,
//! the same shape every sibling store in this module takes
//! ([`crate::commands::stopwatch_store::StopwatchHandle`],
//! [`crate::commands::scoreboard_store::ScoreboardHandle`]).
//!
//! # How it works
//!
//! [`FunctionHandle::load_from`] walks `<world_dir>/datapacks/<pack>/data/
//! <namespace>/function/**/*.mcfunction` — the registry-keyed directory
//! vanilla's own `Registries.elementsDirPath` resolves `minecraft:function`
//! to (`.cache/mc/26.2/src/net/minecraft/server/ServerFunctionLibrary.java`),
//! **not** the pre-1.21 `functions/` (plural) folder — and the matching
//! `data/<namespace>/tags/function/**/*.json` tag files, flattening a tag's
//! `#other:tag` entries into the concrete function ids they eventually name
//! (cycle-safe: a tag that (transitively) names itself simply stops
//! contributing further entries rather than looping). Datapack directories
//! are visited in sorted order — a v1 simplification of vanilla's own
//! `pack.mcmeta`-driven priority, undocumented anywhere in this crate before
//! now because nothing exercised more than one pack at a time.
//!
//! `/function <name>` and `/reload` (`crate::commands::function`) are this
//! store's only readers; `IntegratedServer`'s persistent-world constructor
//! (`crate::integrated`) is its one production writer, calling
//! [`FunctionHandle::load_from`] once at world-open time with the same
//! `world_dir` its region source already reads from.
//!
//! # What is not built
//!
//! **Macro functions** (`$` lines, `MacroFunction` in vanilla) are read and
//! skipped rather than expanded — `/function` here always calls a function
//! with no `with <storage>`/NBT argument, so there is nowhere for a macro's
//! substitution source to come from yet. A line beginning with `$` is
//! dropped silently, the same way vanilla's own compiler would refuse it
//! outright without a `with` clause; this is a smaller gap since no built-in
//! command surface exists to *supply* one.
//!
//! **No persistence of a "last-reload" timestamp or checksum** — every
//! `/reload` rescans the whole tree unconditionally, matching vanilla's own
//! reload cost model (it is not incremental there either).
//!
//! # Dependencies
//!
//! `std::fs` only — cfg-gated to native, like every other filesystem-backed
//! store in this crate ([`crate::region_source`], [`crate::access`]): a
//! browser singleplayer world has no filesystem and no datapacks directory to
//! read.

#[cfg(not(target_arch = "wasm32"))]
use std::collections::{HashMap, HashSet};
#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// One (re)load's outcome, for `/reload`'s own feedback line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DatapackLoadReport {
    pub functions: usize,
    pub tags: usize,
}

#[derive(Debug, Default)]
struct FunctionLibrary {
    /// `namespace:path` (no leading `#`) -> its command lines, in file order,
    /// comments and blank lines already stripped and line-continuations
    /// already joined.
    functions: std::collections::HashMap<String, Vec<String>>,
    /// `namespace:path` (the tag's own id, `#` stripped) -> the function ids
    /// it names, already flattened through any nested tag references.
    tags: std::collections::HashMap<String, Vec<String>>,
    /// Where this library was last loaded from, so a bare `/reload` knows
    /// what to re-scan. `None` until [`FunctionHandle::load_from`] runs at
    /// least once.
    #[cfg(not(target_arch = "wasm32"))]
    root: Option<PathBuf>,
}

/// A shared handle to one world's loaded datapack functions and function
/// tags — cheap to clone (one `Arc`), riding inside
/// [`crate::world_state::WorldStateHandle`] as a sibling of `scoreboard`/
/// `stopwatches` for the identical reachability reason those two document.
#[derive(Debug, Clone, Default)]
pub struct FunctionHandle(Arc<Mutex<FunctionLibrary>>);

impl FunctionHandle {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Scans `world_dir`'s `datapacks/` folder and replaces the library
    /// wholesale. Remembers `world_dir` so a later bare [`Self::reload`] can
    /// re-run this with no argument, matching vanilla's own `/reload`.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load_from(&self, world_dir: &Path) -> DatapackLoadReport {
        let (functions, tags) = scan(&world_dir.join("datapacks"));
        let report =
            DatapackLoadReport { functions: functions.len(), tags: tags.len() };
        let mut guard = self.0.lock().expect("function library lock poisoned");
        guard.functions = functions;
        guard.tags = tags;
        guard.root = Some(world_dir.to_path_buf());
        report
    }

    /// Re-scans whatever directory the last [`Self::load_from`] used —
    /// `/reload`'s own entry point. `None` (rather than a report of zero)
    /// when nothing has ever been loaded: RCON's own world source and every
    /// in-memory/browser world have no datapacks directory to re-read at
    /// all, and that is a different, honester answer than "reloaded zero".
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn reload(&self) -> Option<DatapackLoadReport> {
        let root = self.0.lock().expect("function library lock poisoned").root.clone()?;
        Some(self.load_from(&root))
    }

    /// The wasm32/no-filesystem answer: there is never anything to reload.
    #[cfg(target_arch = "wasm32")]
    #[must_use]
    pub fn reload(&self) -> Option<DatapackLoadReport> {
        None
    }

    /// One function's command lines by fully-qualified id (`namespace:path`,
    /// no leading `#`). `None` when no loaded datapack declares it —
    /// [`crate::commands::function`]'s hard-refusal case for a bare
    /// `/function <name>`.
    #[must_use]
    pub fn function(&self, id: &str) -> Option<Vec<String>> {
        self.0.lock().expect("function library lock poisoned").functions.get(id).cloned()
    }

    /// A tag's member function ids by fully-qualified id (no leading `#`),
    /// or an empty list for an undeclared tag — matching vanilla's own
    /// `getTag`'s `getOrDefault(tag, List.of())`, which is *not* an error the
    /// way an unknown single function is.
    #[must_use]
    pub fn tag(&self, id: &str) -> Vec<String> {
        self.0.lock().expect("function library lock poisoned").tags.get(id).cloned().unwrap_or_default()
    }

    /// Every loaded function id, for test/debug enumeration — mirrors
    /// [`crate::worldgen_data::embedded_structure_template_ids`]'s "walk the
    /// whole corpus" shape rather than a hand-picked list.
    #[must_use]
    pub fn function_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> =
            self.0.lock().expect("function library lock poisoned").functions.keys().cloned().collect();
        ids.sort();
        ids
    }
}

/// Scans `datapacks_dir` (a world's `datapacks/` folder, which may not
/// exist — an empty result then, not an error) and returns
/// `(functions, tags)`. Datapack subdirectories are visited in sorted order;
/// a later pack's function silently replaces an earlier pack's same id,
/// matching vanilla's own "last pack wins" resource-pack merge rule.
#[cfg(not(target_arch = "wasm32"))]
fn scan(datapacks_dir: &Path) -> (HashMap<String, Vec<String>>, HashMap<String, Vec<String>>) {
    let mut functions: HashMap<String, Vec<String>> = HashMap::new();
    // Raw, unflattened tag entries: each string is either a bare function id
    // or a `#`-prefixed reference to another tag.
    let mut raw_tags: HashMap<String, Vec<String>> = HashMap::new();

    let Ok(mut packs) = std::fs::read_dir(datapacks_dir).map(|entries| {
        let mut names: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_dir()).collect();
        names.sort();
        names
    }) else {
        return (functions, raw_tags);
    };
    packs.sort();

    for pack in packs {
        let data_dir = pack.join("data");
        let Ok(namespaces) = std::fs::read_dir(&data_dir) else { continue };
        let mut namespace_dirs: Vec<PathBuf> =
            namespaces.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_dir()).collect();
        namespace_dirs.sort();

        for namespace_dir in namespace_dirs {
            let Some(namespace) = namespace_dir.file_name().and_then(|n| n.to_str()) else { continue };
            let namespace = namespace.to_string();

            scan_functions(&namespace_dir.join("function"), &namespace, &mut functions);
            scan_tags(&namespace_dir.join("tags").join("function"), &namespace, &mut raw_tags);
        }
    }

    let tags = flatten_tags(&raw_tags, &functions);
    (functions, tags)
}

/// Recursively collects every `*.mcfunction` file under `dir` into `out`,
/// keyed `<namespace>:<relative path, '/'-joined, no extension>` — vanilla's
/// own `FileToIdConverter` restated.
#[cfg(not(target_arch = "wasm32"))]
fn scan_functions(dir: &Path, namespace: &str, out: &mut HashMap<String, Vec<String>>) {
    walk(dir, "mcfunction", &mut |relative, contents| {
        let id = format!("{namespace}:{relative}");
        out.insert(id, parse_function_lines(contents));
    });
}

/// Recursively collects every `*.json` tag file under `dir` into `out`,
/// keyed the same way [`scan_functions`] keys a function.
#[cfg(not(target_arch = "wasm32"))]
fn scan_tags(dir: &Path, namespace: &str, out: &mut HashMap<String, Vec<String>>) {
    walk(dir, "json", &mut |relative, contents| {
        let id = format!("{namespace}:{relative}");
        let entries = parse_tag_json(contents);
        out.entry(id).or_default().extend(entries);
    });
}

/// Walks `dir` recursively, calling `f(relative_path_no_ext, file_contents)`
/// for every file whose extension is `ext`. Silent on any I/O error for an
/// individual entry (a directory that vanished mid-scan, a permission
/// error) — the same "best effort, never a hard failure" shape
/// [`crate::worldgen_data`]'s own structure-template loader takes for a
/// bundle that is not required to exist.
#[cfg(not(target_arch = "wasm32"))]
fn walk(dir: &Path, ext: &str, f: &mut dyn FnMut(&str, &str)) {
    fn inner(root: &Path, dir: &Path, ext: &str, f: &mut dyn FnMut(&str, &str)) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                inner(root, &path, ext, f);
            } else if path.extension().and_then(|e| e.to_str()) == Some(ext) {
                let Ok(contents) = std::fs::read_to_string(&path) else { continue };
                let Ok(relative) = path.strip_prefix(root) else { continue };
                let relative = relative.with_extension("");
                let relative = relative.components().filter_map(|c| c.as_os_str().to_str()).collect::<Vec<_>>().join("/");
                f(&relative, &contents);
            }
        }
    }
    inner(dir, dir, ext, f);
}

/// `CommandFunction.fromLines`, restated for our own dispatcher rather than
/// Brigadier's: strips `#`-comments and blank lines, joins a trailing-`\`
/// line continuation, and skips a `$`-prefixed macro line (see this module's
/// doc for why macros are not expanded). Unlike vanilla, a malformed line is
/// never a hard load-time error here — an unparseable command is simply run
/// (and refused) the same as any other line, at *execution* time, since this
/// crate's own dispatcher already reports a clean per-line refusal rather
/// than needing a separate compile pass.
#[cfg(not(target_arch = "wasm32"))]
fn parse_function_lines(contents: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut pending: Option<String> = None;
    for raw in contents.lines() {
        let line = raw.trim();
        let line = match pending.take() {
            Some(mut buf) => {
                buf.push_str(line);
                buf
            }
            None => line.to_string(),
        };
        if let Some(stripped) = line.strip_suffix('\\') {
            pending = Some(stripped.to_string());
            continue;
        }
        if line.is_empty() || line.starts_with('#') || line.starts_with('$') {
            continue;
        }
        lines.push(line);
    }
    // An unterminated continuation at EOF: run whatever was accumulated
    // rather than silently dropping it.
    if let Some(buf) = pending {
        if !buf.is_empty() {
            lines.push(buf);
        }
    }
    lines
}

/// A function tag JSON's `values` array — each entry either a bare string
/// (`"minecraft:foo"`) or vanilla's newer `{"id": "...", "required": false}`
/// object form. `replace` is not honoured (this v1 always merges rather than
/// letting one pack clear an earlier pack's entries) — a documented
/// simplification, not an oversight.
#[cfg(not(target_arch = "wasm32"))]
fn parse_tag_json(contents: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(contents) else { return Vec::new() };
    let Some(values) = value.get("values").and_then(|v| v.as_array()) else { return Vec::new() };
    values
        .iter()
        .filter_map(|entry| match entry {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Object(obj) => obj.get("id").and_then(|id| id.as_str()).map(str::to_string),
            _ => None,
        })
        .collect()
}

/// Resolves every tag's raw entries (bare function ids and `#tag`
/// references) down to concrete function ids, following nested tag
/// references with a visited set so a cycle simply stops contributing
/// further entries instead of looping — vanilla's own `TagLoader` does the
/// equivalent with a `visited` set in `TagLoader.build`.
#[cfg(not(target_arch = "wasm32"))]
fn flatten_tags(
    raw: &HashMap<String, Vec<String>>,
    functions: &HashMap<String, Vec<String>>,
) -> HashMap<String, Vec<String>> {
    fn resolve(
        id: &str,
        raw: &HashMap<String, Vec<String>>,
        functions: &HashMap<String, Vec<String>>,
        visiting: &mut HashSet<String>,
        out: &mut Vec<String>,
        seen: &mut HashSet<String>,
    ) {
        if !visiting.insert(id.to_string()) {
            // Cycle: this tag is already being expanded higher up the call
            // stack. Stop here rather than looping.
            return;
        }
        if let Some(entries) = raw.get(id) {
            for entry in entries {
                if let Some(nested) = entry.strip_prefix('#') {
                    resolve(nested, raw, functions, visiting, out, seen);
                } else if functions.contains_key(entry) && seen.insert(entry.clone()) {
                    out.push(entry.clone());
                }
                // An entry naming neither a real function nor a resolvable
                // tag is dropped — vanilla's own optional-entry behaviour
                // (`required: false` is the default) for a reference the
                // loaded datapacks do not actually provide.
            }
        }
        visiting.remove(id);
    }

    raw.keys()
        .map(|id| {
            let mut out = Vec::new();
            let mut seen = HashSet::new();
            resolve(id, raw, functions, &mut HashSet::new(), &mut out, &mut seen);
            (id.clone(), out)
        })
        .collect()
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::fs;

    use super::*;

    /// Writes `path` (relative to a temp datapack root) with `contents`,
    /// creating parent directories as needed.
    fn write(root: &Path, path: &str, contents: &str) {
        let full = root.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, contents).unwrap();
    }

    /// A unique scratch directory per test, so concurrent test runs never
    /// collide on the same path.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("lodestone-function-store-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_real_mcfunction_file_under_a_real_datapack_directory_loads_with_comments_and_blanks_stripped() {
        let world = scratch("basic");
        write(
            &world,
            "datapacks/pack/data/test/function/greet.mcfunction",
            "# a comment\n\nsay hello\nsay world\n",
        );
        let handle = FunctionHandle::new();
        let report = handle.load_from(&world);
        assert_eq!(report, DatapackLoadReport { functions: 1, tags: 0 });
        assert_eq!(
            handle.function("test:greet"),
            Some(vec!["say hello".to_string(), "say world".to_string()])
        );
        assert_eq!(handle.function("test:missing"), None, "an unknown function must answer None, not an empty Vec");
    }

    #[test]
    fn a_trailing_backslash_continues_onto_the_next_line() {
        let world = scratch("continuation");
        write(
            &world,
            "datapacks/pack/data/test/function/multi.mcfunction",
            "say this is one \\\ncommand split across lines\n",
        );
        let handle = FunctionHandle::new();
        handle.load_from(&world);
        assert_eq!(
            handle.function("test:multi"),
            Some(vec!["say this is one command split across lines".to_string()])
        );
    }

    #[test]
    fn a_tag_flattens_nested_tags_and_drops_unresolvable_entries_without_erroring() {
        let world = scratch("tags");
        write(&world, "datapacks/pack/data/test/function/a.mcfunction", "say a\n");
        write(&world, "datapacks/pack/data/test/function/b.mcfunction", "say b\n");
        write(
            &world,
            "datapacks/pack/data/test/tags/function/inner.json",
            r#"{"values": ["test:a", "test:does_not_exist"]}"#,
        );
        write(
            &world,
            "datapacks/pack/data/test/tags/function/outer.json",
            r##"{"values": ["test:b", "#test:inner"]}"##,
        );
        let handle = FunctionHandle::new();
        let report = handle.load_from(&world);
        assert_eq!(report.tags, 2);
        let mut outer = handle.tag("test:outer");
        outer.sort();
        assert_eq!(outer, vec!["test:a".to_string(), "test:b".to_string()]);
        assert_eq!(handle.tag("test:undeclared"), Vec::<String>::new(), "an unknown tag is empty, not an error");
    }

    #[test]
    fn a_self_referencing_tag_does_not_hang_and_still_reports_its_real_members() {
        let world = scratch("cycle");
        write(&world, "datapacks/pack/data/test/function/a.mcfunction", "say a\n");
        write(
            &world,
            "datapacks/pack/data/test/tags/function/cycle.json",
            r##"{"values": ["test:a", "#test:cycle"]}"##,
        );
        let handle = FunctionHandle::new();
        handle.load_from(&world);
        assert_eq!(handle.tag("test:cycle"), vec!["test:a".to_string()]);
    }

    #[test]
    fn reload_rescans_the_same_root_and_picks_up_a_newly_added_function() {
        let world = scratch("reload");
        write(&world, "datapacks/pack/data/test/function/a.mcfunction", "say a\n");
        let handle = FunctionHandle::new();
        handle.load_from(&world);
        assert_eq!(handle.function_ids(), vec!["test:a".to_string()]);

        write(&world, "datapacks/pack/data/test/function/b.mcfunction", "say b\n");
        let report = handle.reload().expect("a root was already loaded");
        assert_eq!(report.functions, 2);
        assert_eq!(handle.function_ids(), vec!["test:a".to_string(), "test:b".to_string()]);
    }

    #[test]
    fn reload_with_no_prior_load_from_answers_none_rather_than_a_zero_report() {
        let handle = FunctionHandle::new();
        assert_eq!(handle.reload(), None);
    }

    #[test]
    fn a_missing_datapacks_directory_loads_as_empty_rather_than_erroring() {
        let world = scratch("no-datapacks");
        let handle = FunctionHandle::new();
        let report = handle.load_from(&world);
        assert_eq!(report, DatapackLoadReport::default());
    }
}
