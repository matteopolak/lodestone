//! `nms-census` — report every `net.minecraft.*` member a jar's bytecode
//! contains, with static instruction-site counts and field directions.
//!
//! ```text
//! cargo run -p lodestone-nms-census --bin nms-census -- <jar> [flags]
//! ```
//!
//! See [`lodestone_nms_census`] for what the four populations mean and why
//! "who is referring" is the number that matters. `--help` lists the flags.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use lodestone_nms_census::{Census, ScanOptions};

const USAGE: &str = "\
nms-census — census the NMS member instruction sites a jar contains

USAGE:
    nms-census <jar> [OPTIONS]

OPTIONS:
    --prefix <P>          Internal-form package to census, with trailing slash
                          [default: net/minecraft/]
    --internal <P>        A prefix whose classes count as part of the layer
                          being replaced, so references *from* them are
                          internal rather than external. Repeatable.
                          [default: the value of --prefix]
    --no-recurse          Do not descend into nested .jar entries. Both the
                          Mojang bundler and Paper's paperclip launcher hide
                          the real classes one level down, so this will report
                          almost nothing on either.
    --top <N>             How many rows per table [default: 40]
    --all                 Report every row rather than the top N
    --classes-only        Report the per-class summary and skip member rows
    -h, --help            This text
";

struct Args {
    jar: PathBuf,
    options: ScanOptions,
    top: usize,
    all: bool,
    classes_only: bool,
}

fn parse_args() -> Result<Option<Args>> {
    let mut jar: Option<PathBuf> = None;
    let mut prefix = "net/minecraft/".to_owned();
    let mut internal: Vec<String> = Vec::new();
    let mut recurse = true;
    let mut top = 40usize;
    let mut all = false;
    let mut classes_only = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--no-recurse" => recurse = false,
            "--all" => all = true,
            "--classes-only" => classes_only = true,
            "--prefix" => {
                prefix = args.next().context("--prefix needs a value")?;
            }
            "--internal" => {
                internal.push(args.next().context("--internal needs a value")?);
            }
            "--top" => {
                top = args
                    .next()
                    .context("--top needs a value")?
                    .parse()
                    .context("--top must be a number")?;
            }
            other if other.starts_with('-') => bail!("unknown flag {other}\n\n{USAGE}"),
            other => {
                if jar.is_some() {
                    bail!("only one jar at a time\n\n{USAGE}");
                }
                jar = Some(PathBuf::from(other));
            }
        }
    }

    let jar = jar.context("no jar given")?;
    // A prefix without its trailing slash silently widens the census —
    // `net/minecraft` also matches `net/minecraftforge/`. Refuse rather than
    // report a number nobody can interpret.
    if !prefix.ends_with('/') {
        bail!("--prefix must end with '/' (got {prefix:?}); without it the match is a wider package than you meant");
    }
    if internal.is_empty() {
        internal.push(prefix.clone());
    }
    Ok(Some(Args {
        jar,
        options: ScanOptions {
            target_prefix: prefix,
            internal_prefixes: internal,
            recurse_jars: recurse,
        },
        top,
        all,
        classes_only,
    }))
}

fn main() -> Result<()> {
    let Some(args) = parse_args()? else {
        print!("{USAGE}");
        return Ok(());
    };

    let census = Census::scan_jar(&args.jar, &args.options)?;
    let limit = if args.all { usize::MAX } else { args.top };

    println!("jar:                 {}", args.jar.display());
    println!("target prefix:       {}", args.options.target_prefix);
    println!(
        "internal prefixes:   {}",
        args.options.internal_prefixes.join(", ")
    );
    println!("archives opened:     {}", census.jars_scanned);
    println!("classes parsed:      {}", census.classes_scanned);
    println!(
        "parse failures:      {}{}",
        census.parse_failure_count(),
        if census.parse_failure_count() == 0 {
            ""
        } else {
            "  (examples below)"
        }
    );
    println!(
        "target classes DEFINED in this jar: {}",
        census.defined_target_classes.len()
    );

    let external_members = census.external_members();
    let external_uses: u64 = external_members.iter().map(|(_, s)| s.external).sum();
    let total_uses: u64 = census.members.values().map(|s| s.total()).sum();
    let external_symbolic = census.external_symbolic_members();
    let external_classes = census.external_classes();

    println!();
    println!("== static Code instruction surface an external caller reaches for ==");
    println!("distinct member operations:    {}", external_members.len());
    println!("distinct classes touched:      {}", external_classes.len());
    println!("external static instruction sites: {external_uses}");
    println!(
        "(all static instruction sites, incl. internal: {} across {} member operations)",
        total_uses,
        census.members.len()
    );
    println!(
        "symbolic pool members (external): {} (kept separately; not static-site counts)",
        external_symbolic.len()
    );
    println!(
        "classes named in a `new`/cast/catch: {}",
        census.types.values().filter(|s| s.external > 0).count()
    );
    println!(
        "classes named only in a descriptor:  {}",
        census
            .descriptor_types
            .iter()
            .filter(|(name, stat)| stat.external > 0
                && !external_classes.contains_key(name.as_str()))
            .count()
    );

    if !external_classes.is_empty() {
        println!();
        println!("== classes with most external static sites ==");
        let mut rows: Vec<_> = external_classes.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        for (class, count) in rows.iter().take(limit) {
            println!("{count:>8}  {class}");
        }
        if rows.len() > limit {
            println!("         … and {} more", rows.len() - limit);
        }
    }

    if !args.classes_only && !external_members.is_empty() {
        println!();
        println!("== member operations with most external static sites ==");
        for (key, stat) in external_members.iter().take(limit) {
            println!(
                "{:>8}  {}  {}.{}{}",
                stat.external,
                key.kind.label(),
                key.class,
                key.name,
                key.descriptor
            );
        }
        if external_members.len() > limit {
            println!("         … and {} more", external_members.len() - limit);
        }
    }

    if !census.parse_failures.is_empty() {
        println!();
        println!("== parse failures (first {}) ==", census.parse_failures.len());
        for (origin, reason) in &census.parse_failures {
            println!("  {origin}: {reason}");
        }
    }

    Ok(())
}
