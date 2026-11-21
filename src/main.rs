use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod analysis;
mod contracts;
mod vars;
mod walk;

const HEADER: &str = include_str!("../include/spacelizard.dm");

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Analyze every selected preprocessor pass.
    Check {
        #[arg(long, default_value = "spacelizard.toml")]
        config: PathBuf,
        #[arg(long)]
        ci: bool,
        #[arg(long)]
        verify_header: bool,
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
        /// Add NAME or NAME=VALUE to every pass.
        #[arg(short = 'D', long = "define")]
        defines: Vec<String>,
        /// Print wall-clock phase timings to stderr.
        #[arg(long)]
        timings: bool,
    },
    /// Write the embedded DM header.
    SyncHeader { path: PathBuf },
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Text,
    Github,
    Json,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Severity {
    Off,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct Diagnostic {
    file: String,
    line: u32,
    column: u16,
    rule: &'static str,
    message: String,
    severity: Severity,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    environment: PathBuf,
    #[serde(default = "default_header_path")]
    header_path: PathBuf,
    #[serde(default)]
    passes: Passes,
    #[serde(default)]
    rules: BTreeMap<String, Severity>,
    #[serde(default, rename = "forwarding")]
    _forwarding: Vec<toml::Table>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Passes {
    default: Vec<String>,
    ci_extra: Vec<Vec<String>>,
}

fn default_header_path() -> PathBuf {
    "code/__DEFINES/spacelizard.dm".into()
}

struct PassResult {
    diagnostics: Vec<Diagnostic>,
    parse: Duration,
    analysis: Duration,
    walked: usize,
    total: usize,
}

fn run() -> Result<bool, Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let Command::Check {
        config,
        ci,
        verify_header,
        format,
        defines,
        timings,
    } = cli.command
    else {
        let Command::SyncHeader { path } = cli.command else {
            unreachable!()
        };
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, HEADER)?;
        return Ok(false);
    };

    let started = Instant::now();
    let root = config.parent().unwrap_or(Path::new(""));
    let config: Config = toml::from_str(&std::fs::read_to_string(&config)?)?;
    if verify_header {
        let path = root.join(&config.header_path);
        let contents = std::fs::read(&path).map_err(|error| {
            format!("{}: {error}. Run spacelizard sync-header.", path.display())
        })?;
        if contents != HEADER.as_bytes() {
            return Err(format!(
                "{} differs from the embedded header. Run spacelizard sync-header.",
                path.display()
            )
            .into());
        }
    }

    let environment = root.join(&config.environment);
    let mut passes = vec![("default".to_owned(), config.passes.default)];
    if ci {
        passes.extend(
            config
                .passes
                .ci_extra
                .into_iter()
                .enumerate()
                .map(|(i, p)| (format!("ci_extra[{}]", i + 1), p)),
        );
    }

    let results = std::thread::scope(|scope| {
        let threads: Result<Vec<_>, String> = passes
            .into_iter()
            .map(|(label, pass)| {
                let input = analysis::define_source(pass.iter().chain(&defines))?;
                let environment = &environment;
                let rules = &config.rules;
                Ok((
                    label,
                    scope.spawn(move || analysis::run(environment, input, rules)),
                ))
            })
            .collect();
        threads?
            .into_iter()
            .map(|(label, thread)| {
                let result = thread
                    .join()
                    .map_err(|_| format!("parser panicked in {label}"))??;
                Ok((label, result))
            })
            .collect::<Result<Vec<_>, String>>()
    })?;

    let output_started = Instant::now();
    let mut merged = BTreeMap::new();
    for (label, result) in results {
        if timings {
            eprintln!(
                "spacelizard {label} preprocess_parse={:.6}s analysis={:.6}s walked={}/{}",
                result.parse.as_secs_f64(),
                result.analysis.as_secs_f64(),
                result.walked,
                result.total
            );
        }
        let mut counts = BTreeMap::new();
        for diagnostic in result.diagnostics {
            *counts.entry(diagnostic).or_insert(0usize) += 1;
        }
        for (diagnostic, count) in counts {
            let max_count = merged.entry(diagnostic).or_insert(0usize);
            *max_count = (*max_count).max(count);
        }
    }
    let diagnostics: Vec<_> = merged
        .into_iter()
        .flat_map(|(diagnostic, count)| std::iter::repeat_n(diagnostic, count))
        .collect();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    output(&mut stdout, &diagnostics, format)?;
    stdout.flush()?;
    if timings {
        eprintln!(
            "spacelizard output={:.6}s total={:.6}s",
            output_started.elapsed().as_secs_f64(),
            started.elapsed().as_secs_f64()
        );
    }
    Ok(diagnostics.iter().any(|d| d.severity == Severity::Error))
}

fn output(writer: &mut impl Write, diagnostics: &[Diagnostic], format: Format) -> io::Result<()> {
    if matches!(format, Format::Json) {
        serde_json::to_writer_pretty(&mut *writer, diagnostics)?;
        return writeln!(writer);
    }
    for d in diagnostics {
        let level = if d.severity == Severity::Warning {
            "warning"
        } else {
            "error"
        };
        let message = d.message.replace(['\n', '\r'], " ");
        match format {
            Format::Text => writeln!(
                writer,
                "{}:{}:{}: {level}[{}]: {message}",
                d.file, d.line, d.column, d.rule
            )?,
            Format::Github => {
                let file = github_escape(&d.file)
                    .replace(',', "%2C")
                    .replace(':', "%3A");
                let message = github_escape(&d.message);
                writeln!(
                    writer,
                    "::{level} file={file},line={},col={},title=spacelizard [{}]::{message}",
                    d.line, d.column, d.rule
                )?;
            }
            Format::Json => unreachable!(),
        }
    }
    Ok(())
}

fn github_escape(text: &str) -> String {
    text.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

fn main() {
    match run() {
        Ok(false) => {}
        Ok(true) => std::process::exit(1),
        Err(error) => {
            eprintln!("spacelizard: {error}");
            std::process::exit(1);
        }
    }
}
