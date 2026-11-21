use crate::{Diagnostic, PassResult, Severity, contracts, vars, walk};
use dreammaker::ast::{Expression, Ident, Term};
use dreammaker::lexer::{LocatedToken, Token};
use dreammaker::objtree::ProcRef;
use dreammaker::{Context, Location, Parser, Preprocessor};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const RULE_DECL: &str = "__spacelizard_var_rule";
pub const RULE_CALL: &str = "__spacelizard_rule";
pub const VAR_BYPASS: &str = "__spacelizard_bypass_var_rule";
pub const BLOCK_BYPASS: &str = "__spacelizard_bypass_block";
pub const PARAM_SKIP: &str = "__spacelizard_skip_param";
pub const RETURN_TYPE: &str = "SpacemanDMM_return_type";

pub type Issue = (&'static str, String);

pub fn define_source<'a>(defines: impl Iterator<Item = &'a String>) -> Result<String, String> {
    let mut ordered: Vec<(&str, &str)> = Vec::new();
    let mut positions: HashMap<&str, usize> = HashMap::new();
    for define in defines {
        let (name, value) = define.split_once('=').unwrap_or((define, "1"));
        let mut chars = name.chars();
        let valid = chars
            .next()
            .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
            && chars.all(|c| c == '_' || c.is_ascii_alphanumeric());
        if !valid || value.contains(['\r', '\n']) || value.ends_with('\\') || name == "SPACELIZARD"
        {
            return Err(format!("invalid preprocessor define {define:?}"));
        }
        if let Some(&idx) = positions.get(name) {
            ordered[idx].1 = value;
        } else {
            positions.insert(name, ordered.len());
            ordered.push((name, value));
        }
    }
    let mut source = "#define SPACELIZARD 1\n".to_owned();
    for (name, value) in ordered {
        source.push_str(&format!("#define {name} {value}\n"));
    }
    Ok(source)
}

#[derive(Default)]
struct Tokens(HashMap<Ident, Vec<Location>>);

impl Tokens {
    fn record(&mut self, token: &LocatedToken) {
        if let Token::Ident(name, _) | Token::String(name, _) = &token.token {
            self.0.entry(name.clone()).or_default().push(token.start);
        }
    }

    fn finish(&mut self) {
        for positions in self.0.values_mut() {
            positions.sort_unstable();
            positions.dedup();
        }
    }

    fn in_proc(&self, proc: ProcRef<'_>, names: &[&str]) -> bool {
        let Some(range) = &proc.body_range else {
            return true;
        };
        // Compare whole lines to account for punctuation and tokens sharing the
        // position of a surrounding macro invocation.
        let start = Location {
            column: 0,
            ..range.start
        };
        let end = Location {
            column: u16::MAX,
            ..range.end
        };
        start.file != end.file
            || names.iter().any(|name| {
                self.0.get(*name).is_some_and(|positions| {
                    positions
                        .get(positions.partition_point(|p| *p < start))
                        .is_some_and(|p| *p <= end)
                })
            })
    }
}

pub struct Reporter<'a> {
    context: &'a Context,
    rules: &'a BTreeMap<String, Severity>,
    macro_references: HashSet<(u64, String)>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Reporter<'_> {
    pub fn add(&mut self, loc: Location, id: &'static str, message: impl Into<String>) {
        let default = if id == "var_alias_escape" {
            Severity::Warning
        } else {
            Severity::Error
        };
        self.with_severity(loc, id, message, default);
    }

    fn with_severity(
        &mut self,
        loc: Location,
        id: &'static str,
        message: impl Into<String>,
        default: Severity,
    ) {
        let severity = if id == "dm_parse" {
            default
        } else {
            self.rules.get(id).copied().unwrap_or(default)
        };
        if severity == Severity::Off {
            return;
        }
        let message = message.into();
        // INVOKE_ASYNC expands its proc reference into both arms. Report the
        // source reference once, after skip handling has visited each AST node.
        if id == "proc_reference" && !self.macro_references.insert((loc.pack(), message.clone())) {
            return;
        }
        let file = self
            .context
            .file_path(loc.file)
            .to_string_lossy()
            .replace('\\', "/");
        self.diagnostics.push(Diagnostic {
            file,
            line: loc.line,
            column: loc.column,
            rule: id,
            message,
            severity,
        });
    }
}

pub fn run(
    path: &Path,
    defines: String,
    rules: &BTreeMap<String, Severity>,
) -> Result<PassResult, String> {
    let started = Instant::now();
    let context = Context::default();
    let mut input = Preprocessor::new(&context, path.to_owned()).map_err(|e| e.to_string())?;
    input
        .push_file(PathBuf::from("<spacelizard-defines>"), Cursor::new(defines))
        .map_err(|e| e.to_string())?;
    let mut tokens = Tokens::default();
    let mut parser = Parser::new(&context, input.inspect(|token| tokens.record(token)));
    parser.enable_procs();
    let (fatal, tree) = parser.parse_object_tree_2();
    let parse = started.elapsed();
    let started = Instant::now();
    let mut reporter = Reporter {
        context: &context,
        rules,
        macro_references: HashSet::new(),
        diagnostics: Vec::new(),
    };
    tokens.finish();

    for error in context.errors().iter() {
        use dreammaker::Severity as Dm;
        let severity = match error.severity() {
            Dm::Error => Severity::Error,
            Dm::Warning => Severity::Warning,
            _ => continue,
        };
        let location = error.location();
        let misplaced_marker = severity == Severity::Error
            && error.description().starts_with("undefined var ")
            && contracts::MARKERS.iter().any(|marker| {
                error.description().contains(&format!("'{marker}'"))
                    && tokens
                        .0
                        .get(*marker)
                        .is_some_and(|locations| locations.binary_search(&location).is_ok())
            });
        if misplaced_marker {
            reporter.add(
                location,
                "bad_marker",
                "call contract marker must be inside a proc",
            );
        } else {
            reporter.with_severity(location, "dm_parse", error.description(), severity);
        }
    }
    if fatal
        && !reporter
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    {
        return Err(format!(
            "{}: parser stopped before reaching end of input",
            path.display()
        ));
    }

    let mut total = 0;
    let mut walked = 0;
    if !fatal {
        let mut contracts = contracts::Contracts::default();
        let mut variables = vars::Rules::default();
        let mut procedures = Vec::new();
        for ty in tree.iter_types() {
            for proc in ty.iter_self_procs().filter(|p| !p.is_builtin()) {
                if proc.name() == RULE_DECL {
                    variables.collect(proc, &mut reporter);
                    continue;
                }
                procedures.push(proc);
                if tokens.in_proc(proc, &contracts::MARKERS) || tokens.in_proc(proc, &[RETURN_TYPE])
                {
                    contracts.collect(proc, &mut reporter);
                }
            }
        }
        contracts.index(&procedures);
        let mut candidates: Vec<_> = contracts.names().chain(variables.names()).collect();
        candidates.extend([PARAM_SKIP, VAR_BYPASS, BLOCK_BYPASS]);
        if contracts.is_marked_name("New") {
            candidates.extend(["new", "newlist"]);
        }
        total = procedures.len();
        for proc in procedures {
            let flags = contracts.flags(proc);
            contracts.definition(proc, flags, &mut reporter);
            let needs_implicit_checks = contracts.is_marked_name(proc.name());
            if proc.code.is_none() {
                if !contracts.is_empty() {
                    reporter.add(
                        proc.location,
                        "unparsable_proc",
                        format!("cannot analyze {}", contracts::display(proc)),
                    );
                }
            } else if needs_implicit_checks || tokens.in_proc(proc, &candidates) {
                walked += 1;
                walk::analyze(proc, &contracts, &variables, &mut reporter);
            }
        }
    }

    Ok(PassResult {
        diagnostics: reporter.diagnostics,
        parse,
        analysis: started.elapsed(),
        walked,
        total,
    })
}

pub fn term(expression: &Expression) -> Option<&Term> {
    match expression.as_term()? {
        Term::Expr(inner) => term(inner),
        plain => Some(plain),
    }
}

pub fn string(expression: &Expression) -> Option<&str> {
    match term(expression)? {
        Term::String(s) => Some(s.as_str()),
        _ => None,
    }
}

pub fn keyword(expression: &Expression) -> Option<(Option<&str>, &Expression)> {
    use dreammaker::ast::AssignOp;
    let Expression::AssignOp {
        op: AssignOp::Assign,
        lhs,
        rhs,
    } = expression
    else {
        return None;
    };
    Some((
        term(lhs).and_then(Term::as_kwarg_key).map(Ident::as_str),
        rhs,
    ))
}
