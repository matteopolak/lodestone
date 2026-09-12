use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodegenRatioReport {
    pub families: Vec<CodegenRatioFamily>,
}

impl CodegenRatioReport {
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::from(
            "protocol codegen ratio\n\
             note: the per-struct ratio is optimistic because one derive block can replace many lines, while adapter dispatch and bespoke codecs dominate hand-written source.\n\
             family  derive-blocks  manual-impls  struct-derived  generated-lines  hand-written-lines\n",
        );
        for family in &self.families {
            let _ = writeln!(
                out,
                "{:<7} {:>13} {:>13} {:>14} {:>16} {:>19}",
                family.family,
                family.derive_blocks,
                family.manual_impls,
                percent(
                    family.derive_blocks,
                    family.derive_blocks + family.manual_impls
                ),
                family.generated_lines,
                family.hand_written_lines,
            );
        }
        out
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodegenRatioFamily {
    pub family: String,
    pub derive_blocks: usize,
    pub manual_impls: usize,
    pub generated_lines: usize,
    pub hand_written_lines: usize,
}

pub fn codegen_ratio_report(workspace_root: &Path) -> Result<CodegenRatioReport> {
    let protocol_root = workspace_root.join("crates/versions");
    let mut families = Vec::new();
    if !protocol_root.is_dir() {
        return Ok(CodegenRatioReport { families });
    }

    for entry in std::fs::read_dir(&protocol_root)
        .with_context(|| format!("read {}", protocol_root.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let family = entry.file_name().to_string_lossy().into_owned();
        let src = entry.path().join("src");
        if !src.is_dir() {
            continue;
        }
        let mut metrics = CodegenRatioFamily {
            family,
            derive_blocks: 0,
            manual_impls: 0,
            generated_lines: 0,
            hand_written_lines: 0,
        };
        collect_codegen_ratio_source_metrics(&src, &src, &mut metrics)?;
        families.push(metrics);
    }

    families.sort_by(|left, right| {
        natural_family_key(&left.family).cmp(&natural_family_key(&right.family))
    });
    Ok(CodegenRatioReport { families })
}

fn collect_codegen_ratio_source_metrics(
    src_root: &Path,
    dir: &Path,
    metrics: &mut CodegenRatioFamily,
) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_codegen_ratio_source_metrics(src_root, &path, metrics)?;
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let source =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let line_count = source.lines().count();
        if is_under_generated(src_root, &path) {
            metrics.generated_lines += line_count;
        } else {
            metrics.hand_written_lines += line_count;
            metrics.derive_blocks += count_codec_derive_blocks(&source);
            metrics.manual_impls += count_manual_codec_impls(&source);
        }
    }
    Ok(())
}

fn is_under_generated(src_root: &Path, path: &Path) -> bool {
    path.strip_prefix(src_root)
        .ok()
        .and_then(|relative| relative.components().next())
        .is_some_and(
            |component| matches!(component, Component::Normal(name) if name == "generated"),
        )
}

fn count_manual_codec_impls(source: &str) -> usize {
    source.matches("impl Encode for").count() + source.matches("impl Decode for").count()
}

fn count_codec_derive_blocks(source: &str) -> usize {
    let mut count = 0;
    let mut rest = source;
    while let Some(start) = rest.find("#[derive") {
        rest = &rest[start + "#[derive".len()..];
        let Some(end) = rest.find(")]") else {
            break;
        };
        let derive_body = &rest[..end];
        if derive_body
            .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
            .any(|token| token == "Encode" || token == "Decode")
        {
            count += 1;
        }
        rest = &rest[end + 2..];
    }
    count
}

fn percent(numerator: usize, denominator: usize) -> String {
    if denominator == 0 {
        "n/a".to_owned()
    } else {
        format!("{:.0}%", (numerator as f64 / denominator as f64) * 100.0)
    }
}

fn natural_family_key(family: &str) -> (u8, u32, u32, &str) {
    if let Some(digits) = family.strip_prefix('v')
        && let Ok(value) = digits.parse::<u32>()
    {
        return (0, value, 0, family);
    }
    // Era-start Minecraft-version directory (`1.8`, `1.9`, `1.14`, `26.2`):
    // compare the major/minor components numerically rather than as a
    // legacy protocol number, so `1.14` does not sort before `1.8`.
    let mut parts = family.split('.').filter_map(|part| part.parse::<u32>().ok());
    if let Some(major) = parts.next() {
        return (1, major, parts.next().unwrap_or(0), family);
    }
    (2, 0, 0, family)
}

/// Options for scaffolding a new protocol version family (`xtask new-version`).
#[derive(Clone, Debug, Eq, PartialEq)]
