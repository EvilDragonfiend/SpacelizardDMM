use crate::analysis::{Issue, RETURN_TYPE, Reporter, keyword, term};
use dreammaker::ast::{Expression, ProcDeclKind, Statement, Term};
use dreammaker::objtree::{ProcRef, TypeIndex, TypeRef};
use std::collections::HashMap;

pub const MARKERS: [&str; 4] = [
    "__spacelizard_call_all_params",
    "__spacelizard_call_required_params",
    "__spacelizard_state_param_names",
    "__spacelizard_keep_param_order",
];

type Key<'a> = (TypeIndex, &'a str, usize);
type Params<'a> = &'a [dreammaker::ast::Parameter];

#[derive(Clone, Copy, Default)]
pub struct Flags([bool; 4]);

impl Flags {
    pub fn active(self) -> bool {
        self.0.into_iter().any(|enabled| enabled)
    }
}

#[derive(Default)]
pub struct Contracts<'a> {
    explicit: HashMap<Key<'a>, [Option<bool>; 4]>,
    effective: HashMap<Key<'a>, Flags>,
    returns: HashMap<Key<'a>, &'a Expression>,
    marked: HashMap<&'a str, Vec<ProcRef<'a>>>,
}

fn key<'a>(proc: ProcRef<'a>) -> Key<'a> {
    let name = proc.ty().get().procs.get_key_value(proc.name()).unwrap().0;
    (proc.ty().index(), name, proc.index())
}

pub fn display(proc: ProcRef<'_>) -> String {
    let kind = if proc
        .get_declaration()
        .is_some_and(|decl| decl.kind == ProcDeclKind::Verb)
    {
        "verb"
    } else {
        "proc"
    };
    format!("{}/{kind}/{}()", proc.ty().get().path, proc.name())
}

impl<'a> Contracts<'a> {
    pub fn collect(&mut self, proc: ProcRef<'a>, out: &mut Reporter<'_>) {
        let mut flags = [None; 4];
        for statement in proc.get().code.as_deref().unwrap_or_default() {
            let Statement::Setting { name, value, .. } = &statement.elem else {
                continue;
            };
            if name.as_str() == RETURN_TYPE {
                self.returns.insert(key(proc), value);
            } else if let Some(index) = MARKERS.iter().position(|m| *m == name.as_str()) {
                let value = match term(value) {
                    Some(Term::Int(0)) => false,
                    Some(Term::Int(1)) => true,
                    _ => {
                        out.add(
                            statement.location,
                            "bad_marker",
                            "expected TRUE, FALSE, 1, or 0",
                        );
                        continue;
                    }
                };
                if flags[index]
                    .replace(value)
                    .is_some_and(|former| former != value)
                {
                    out.add(
                        statement.location,
                        "bad_marker",
                        format!("conflicting contract markers in {}", display(proc)),
                    );
                }
            }
        }
        if flags.iter().any(Option::is_some) {
            self.explicit.insert(key(proc), flags);
        }
    }

    pub fn index(&mut self, procs: &[ProcRef<'a>]) {
        let names: std::collections::HashSet<_> =
            self.explicit.keys().map(|(_, name, _)| *name).collect();
        for &proc in procs.iter().filter(|p| names.contains(p.name())) {
            let mut flags = [None; 4];
            let mut ancestor = Some(proc);
            while let Some(current) = ancestor {
                if let Some(settings) = self.explicit.get(&key(current)) {
                    for (flag, setting) in flags.iter_mut().zip(settings) {
                        *flag = flag.or(*setting);
                    }
                }
                ancestor = current.parent_proc();
            }
            let flags = Flags(flags.map(|flag| flag.unwrap_or_default()));
            if flags.active() {
                self.marked.entry(key(proc).1).or_default().push(proc);
                self.effective.insert(key(proc), flags);
            }
        }
    }

    pub fn flags(&self, proc: ProcRef<'a>) -> Flags {
        self.effective.get(&key(proc)).copied().unwrap_or_default()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.marked.keys().copied()
    }
    pub fn is_empty(&self) -> bool {
        self.marked.is_empty()
    }
    pub fn is_marked_name(&self, name: &str) -> bool {
        self.marked.contains_key(name)
    }

    pub fn definition(&self, proc: ProcRef<'a>, flags: Flags, out: &mut Reporter<'_>) {
        if !flags.active() {
            return;
        }
        let label = display(proc);
        if proc.is_varargs() {
            out.add(
                proc.location,
                "variadic",
                format!("{label} declares a variadic parameter"),
            );
        }
        if flags.0[0] && proc.parameters.iter().any(|p| p.default.is_some()) {
            out.add(
                proc.location,
                "default_value",
                format!("{label} requires all arguments but declares defaults"),
            );
        }
        if proc
            .get_declaration()
            .is_some_and(|decl| decl.kind == ProcDeclKind::Verb)
        {
            out.add(proc.location, "marked_verb", format!("{label} is a verb"));
        }
        if let Some(parent) = proc.parent_proc().filter(|p| !p.is_builtin())
            && !parent
                .parameters
                .iter()
                .filter(|p| p.name.as_str() != "...")
                .enumerate()
                .all(|(index, p)| {
                    proc.parameters
                        .get(index)
                        .is_some_and(|child| child.name == p.name)
                })
        {
            out.add(
                proc.location,
                "override_params",
                format!("{label} must start with its parent's parameter names in order"),
            );
        }
    }

    pub fn return_type(&self, mut proc: ProcRef<'a>) -> Option<&'a Expression> {
        loop {
            if let Some(&expression) = self.returns.get(&key(proc)) {
                return Some(expression);
            }
            proc = proc.parent_proc()?;
        }
    }

    pub fn possibly_marked(&self, ty: Option<TypeRef<'a>>, name: &str) -> bool {
        let Some(procs) = self.marked.get(name) else {
            return false;
        };
        let Some(ty) = ty else { return true };
        ty.get_proc(name).is_some_and(|p| self.flags(p).active())
            || !ty.is_root()
                && procs
                    .iter()
                    .any(|p| p.is_externally_visible() && p.ty().is_subtype_of(ty.get()))
    }

    pub fn call(
        &self,
        ty: Option<TypeRef<'a>>,
        name: &str,
        args: &[Expression],
        exact: bool,
    ) -> Vec<Issue> {
        let Some(procs) = self.marked.get(name) else {
            return Vec::new();
        };
        let Some(ty) = ty else {
            return if name == "New" {
                Vec::new()
            } else {
                vec![(
                    "unverifiable_call",
                    format!("receiver type is unknown for possibly marked proc `{name}`"),
                )]
            };
        };
        let base = ty.get_proc(name);
        let flags = base.map(|p| self.flags(p)).unwrap_or_default();
        let mut issues = base.map_or_else(Vec::new, |p| self.check(p, args));
        if !exact
            && !ty.is_root()
            && procs.iter().any(|p| {
                if !p.is_externally_visible()
                    || p.ty().index() == ty.index()
                    || !p.ty().is_subtype_of(ty.get())
                {
                    return false;
                }
                let derived = self.flags(*p);
                derived
                    .0
                    .into_iter()
                    .zip(flags.0)
                    .any(|(new, old)| new && !old)
                    || derived.0[0] && p.parameters.len() > base.map_or(0, |p| p.parameters.len())
                    || derived.0[1]
                        && required_names(p.parameters.as_ref(), derived).any(|n| {
                            !base.is_some_and(|b| {
                                required_names(b.parameters.as_ref(), flags).any(|m| n == m)
                            })
                        })
            })
        {
            issues.push((
                "unverifiable_call",
                format!(
                    "{} may dispatch `{name}` to a more restrictive override",
                    ty.get().path
                ),
            ));
        }
        issues
    }

    pub fn check(&self, proc: ProcRef<'a>, args: &[Expression]) -> Vec<Issue> {
        let flags = self.flags(proc);
        if flags.active() {
            check_arguments(proc, flags, args)
        } else {
            Vec::new()
        }
    }
}

fn required_names(params: Params<'_>, flags: Flags) -> impl Iterator<Item = &str> {
    params
        .iter()
        .filter(move |p| {
            p.name.as_str() != "..." && (flags.0[0] || flags.0[1] && p.default.is_none())
        })
        .map(|p| p.name.as_str())
}

fn check_arguments(proc: ProcRef<'_>, flags: Flags, args: &[Expression]) -> Vec<Issue> {
    let [all, required, names, order] = flags.0;
    let callee = display(proc);
    let params: Vec<_> = proc
        .parameters
        .iter()
        .filter(|p| p.name.as_str() != "...")
        .collect();
    let mut issues = Vec::new();
    let mut bound = vec![false; params.len()];
    let mut positional = 0;
    let mut previous = None;
    let mut out_of_order = None;
    let mut seen_keyword = false;
    let mut ambiguous = false;
    for (offset, arg) in args.iter().enumerate() {
        let kw = keyword(arg);
        if matches!(term(kw.map_or(arg, |(_, value)| value)), Some(Term::Call(n, _)) if n.as_str() == "arglist")
        {
            return vec![(
                "arglist",
                format!("arglist() prevents argument checking for {callee}"),
            )];
        }
        let index = if let Some((name, _)) = kw {
            seen_keyword = true;
            let Some(index) = params.iter().position(|p| Some(p.name.as_str()) == name) else {
                issues.push((
                    "unknown_keyword",
                    format!(
                        "argument {} ({}) is not a parameter of {callee}",
                        offset + 1,
                        name.unwrap_or("invalid keyword")
                    ),
                ));
                continue;
            };
            index
        } else {
            if names {
                issues.push((
                    "positional_arg",
                    format!(
                        "argument {} to {callee} must name its parameter",
                        offset + 1
                    ),
                ));
            }
            if seen_keyword {
                issues.push((
                    "positional_after_keyword",
                    format!(
                        "argument {} to {callee} follows a keyword argument",
                        offset + 1
                    ),
                ));
                ambiguous = true;
                continue;
            }
            positional += 1;
            positional - 1
        };
        let Some(slot) = bound.get_mut(index) else {
            continue;
        };
        if std::mem::replace(slot, true) {
            issues.push((
                "duplicate_binding",
                format!(
                    "argument {} binds `{}` again in {callee}",
                    offset + 1,
                    params[index].name
                ),
            ));
            continue;
        }
        if let Some(prev) = previous
            && prev > index
            && out_of_order.is_none()
        {
            out_of_order = Some((prev, index));
        }
        previous = Some(index);
    }
    if !ambiguous {
        if order && let Some((prev, index)) = out_of_order {
            issues.push((
                "param_order",
                format!(
                    "`{}` precedes `{}` in the declaration of {callee}",
                    params[index].name, params[prev].name
                ),
            ));
        }
        if all || required {
            if positional > params.len() {
                issues.push(("too_many_args", format!("{callee} declares {} parameters but received {positional} positional arguments", params.len())));
            }
            let missing: Vec<_> = bound
                .iter()
                .zip(&params)
                .filter(|(bound, p)| !**bound && (all || p.default.is_none()))
                .map(|(_, p)| format!("`{}`", p.name))
                .collect();
            if !missing.is_empty() {
                issues.push((
                    "missing_params",
                    format!("{callee} is missing {}", missing.join(", ")),
                ));
            }
        }
    }
    issues
}
