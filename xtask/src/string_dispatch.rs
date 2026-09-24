//! Closed-registry string-dispatch census.
//!
//! Production code should parse text once and then dispatch on generated or
//! hand-written enums. This scanner reports `match` expressions whose arms use
//! string literals. It is deliberately a census, not yet a repository-wide
//! zero-tolerance gate: existing findings form the migration backlog, while a
//! planted control proves the detector still distinguishes string-pattern
//! dispatch from enum-to-string presentation.

use anyhow::{Context, Result, bail};
use proc_macro2::LineColumn;
use std::path::{Path, PathBuf};
use syn::visit::Visit;
use syn::{ExprMatch, ItemFn, Lit, Pat};

const SCAN_ROOTS: &[&str] = &["crates"];
const MIN_FILES_SCANNED: usize = 500;
const MAX_PARSE_FAILURE_FRACTION: f64 = 0.05;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub file: String,
    pub line: usize,
    pub function: String,
    pub literals: Vec<String>,
    pub category: Category,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Generated,
    TestOrBenchmark,
    Boundary,
    Production,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub files_scanned: usize,
    pub parse_failures: usize,
    pub hits: Vec<Hit>,
}

struct MatchVisitor<'a> {
    file: &'a str,
    function: String,
    hits: &'a mut Vec<Hit>,
}

impl<'ast> Visit<'ast> for MatchVisitor<'_> {
    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        let previous = std::mem::replace(&mut self.function, node.sig.ident.to_string());
        syn::visit::visit_item_fn(self, node);
        self.function = previous;
    }

    fn visit_expr_match(&mut self, node: &'ast ExprMatch) {
        let mut literals = Vec::new();
        for arm in &node.arms {
            collect_string_patterns(&arm.pat, &mut literals);
        }
        if !literals.is_empty() {
            literals.sort();
            literals.dedup();
            let function = self.function.clone();
            self.hits.push(Hit {
                file: self.file.to_owned(),
                line: line(node.match_token.span.start()),
                category: categorize(self.file, &function),
                function,
                literals,
            });
        }
        syn::visit::visit_expr_match(self, node);
    }
}

fn categorize(file: &str, function: &str) -> Category {
    if file.contains("/generated/") || file.contains("/generated_") {
        Category::Generated
    } else if file.contains("/tests/")
        || file.contains("/benches/")
        || file.contains("/examples/")
        || file.contains("-allocbench/")
    {
        Category::TestOrBenchmark
    } else if function.starts_with("parse")
        || function.starts_with("decode")
        || function.starts_with("read_")
        || function == "from_name"
        || function == "from_str"
    {
        Category::Boundary
    } else {
        Category::Production
    }
}

fn line(position: LineColumn) -> usize {
    position.line.max(1)
}

fn collect_string_patterns(pattern: &Pat, out: &mut Vec<String>) {
    match pattern {
        Pat::Lit(lit) => {
            if let Lit::Str(value) = &lit.lit {
                out.push(value.value());
            }
        }
        Pat::Or(or) => {
            for case in &or.cases {
                collect_string_patterns(case, out);
            }
        }
        Pat::Paren(paren) => collect_string_patterns(&paren.pat, out),
        Pat::Reference(reference) => collect_string_patterns(&reference.pat, out),
        Pat::Slice(slice) => {
            for element in &slice.elems {
                collect_string_patterns(element, out);
            }
        }
        Pat::Struct(structure) => {
            for field in &structure.fields {
                collect_string_patterns(&field.pat, out);
            }
        }
        Pat::Tuple(tuple) => {
            for element in &tuple.elems {
                collect_string_patterns(element, out);
            }
        }
        Pat::TupleStruct(tuple) => {
            for element in &tuple.elems {
                collect_string_patterns(element, out);
            }
        }
        Pat::Type(typed) => collect_string_patterns(&typed.pat, out),
        _ => {}
    }
}

pub fn scan_workspace(workspace_root: &Path) -> Result<Report> {
    let files = collect_files(workspace_root)?;
    if files.len() < MIN_FILES_SCANNED {
        bail!(
            "string-dispatch scan found only {} Rust files (floor: {MIN_FILES_SCANNED}); the walk is broken",
            files.len()
        );
    }
    scan_paths(workspace_root, &files)
}

fn scan_paths(workspace_root: &Path, files: &[PathBuf]) -> Result<Report> {
    let mut hits = Vec::new();
    let mut parse_failures = 0;
    for path in files {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        let Ok(ast) = syn::parse_file(&text) else {
            parse_failures += 1;
            continue;
        };
        let file = path
            .strip_prefix(workspace_root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let mut visitor = MatchVisitor {
            file: &file,
            function: "<module>".to_owned(),
            hits: &mut hits,
        };
        visitor.visit_file(&ast);
    }
    if parse_failures as f64 / files.len() as f64 > MAX_PARSE_FAILURE_FRACTION {
        bail!(
            "string-dispatch scan failed to parse {parse_failures}/{} files; result is unreliable",
            files.len()
        );
    }
    hits.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    Ok(Report {
        files_scanned: files.len(),
        parse_failures,
        hits,
    })
}

pub fn run_check(workspace_root: &Path) -> Result<()> {
    let report = scan_workspace(workspace_root)?;
    for hit in &report.hits {
        println!(
            "{} {}:{} {} {:?}",
            format!("{:?}", hit.category).to_ascii_uppercase(),
            hit.file,
            hit.line,
            hit.function,
            hit.literals
        );
    }
    let production = report
        .hits
        .iter()
        .filter(|hit| hit.category == Category::Production)
        .count();
    println!(
        "scanned {} Rust files; {} parse failures; {} string-pattern matches; {} production candidates",
        report.files_scanned,
        report.parse_failures,
        report.hits.len(),
        production
    );
    Ok(())
}

fn collect_files(workspace_root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for root in SCAN_ROOTS {
        walk(&workspace_root.join(root), &mut files)?;
    }
    files.sort();
    Ok(files)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("read dir {}", dir.display())),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            if path.file_name().and_then(|name| name.to_str()) != Some("target") {
                walk(&path, out)?;
            }
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_string_patterns_but_not_enum_to_string_output() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("sample.rs");
        std::fs::write(
            &path,
            r#"
                enum Kind { One, Two }
                fn bad(value: &str) { match value { "one" | "two" => (), _ => () } }
                fn display(value: Kind) -> &'static str {
                    match value { Kind::One => "one", Kind::Two => "two" }
                }
            "#,
        )
        .expect("write fixture");
        let report = scan_paths(dir.path(), &[path]).expect("scan fixture");
        assert_eq!(report.hits.len(), 1);
        assert_eq!(report.hits[0].function, "bad");
        assert_eq!(report.hits[0].literals, ["one", "two"]);
        assert_eq!(report.hits[0].category, Category::Production);
    }
}
