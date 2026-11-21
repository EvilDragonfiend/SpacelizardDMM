use crate::analysis::{Issue, RULE_CALL, Reporter, keyword, string, term};
use dreammaker::Location;
use dreammaker::ast::{BinaryOp, Expression, Statement, Term};
use dreammaker::objtree::{ProcRef, TypeRef};
use std::collections::HashMap;

const ASSIGN: u8 = 1;
const LIST_MUTATE: u8 = 2;
const CONSTRUCTOR: u8 = 4;

#[derive(Clone, Copy)]
pub struct Target<'a, 'name> {
    pub owner: Option<TypeRef<'a>>,
    pub member: &'name str,
    pub dynamic: bool,
    pub indexed: bool,
}

#[derive(Clone, Copy)]
pub enum Mutation {
    Assign,
    List,
    Alias,
}

#[derive(Default)]
pub struct Rules<'a>(HashMap<String, Vec<Rule<'a>>>);

enum Allow<'a> {
    Type(TypeRef<'a>),
    Proc(TypeRef<'a>, String),
}

struct Rule<'a> {
    owner: TypeRef<'a>,
    flags: u8,
    allow: Vec<Allow<'a>>,
    hint: Option<String>,
}

impl<'a> Rules<'a> {
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    pub fn collect(&mut self, proc: ProcRef<'a>, out: &mut Reporter<'_>) {
        match Self::decode(proc) {
            Ok((member, rule)) => self.0.entry(member).or_default().push(rule),
            Err(message) => out.add(proc.location, "bad_var_rule", message),
        }
    }

    fn decode(proc: ProcRef<'a>) -> Result<(String, Rule<'a>), String> {
        let [statement] = proc.code.as_deref().unwrap_or_default() else {
            return Err("expected one rule declaration".into());
        };
        let Statement::Expr(expression) = &statement.elem else {
            return Err("expected a rule declaration".into());
        };
        let Some(Term::Call(name, args)) = term(expression) else {
            return Err("expected a rule declaration".into());
        };
        let [target, flags, options @ ..] = args.as_ref() else {
            return Err("expected SET_VAR_RULE(target, flags, ...)".into());
        };
        if name.as_str() != RULE_CALL {
            return Err("expected SET_VAR_RULE".into());
        }
        let target: String = string(target)
            .ok_or("expected a variable name or absolute path")?
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let (owner, member) = if let Some((path, member)) = target.rsplit_once('/') {
            if !target.starts_with('/') {
                return Err("target path must be absolute".into());
            }
            let owner = proc
                .tree()
                .find(path)
                .ok_or_else(|| format!("unknown rule type {path}"))?;
            (owner, member)
        } else {
            (proc.ty(), target.as_str())
        };
        if owner.is_root() || member.is_empty() {
            return Err(format!("invalid variable target {target:?}"));
        }

        let flags = decode_flags(flags)?;
        if flags & (ASSIGN | LIST_MUTATE) == 0 {
            return Err("expected at least one restriction".into());
        }
        let mut rule = Rule {
            owner,
            flags,
            allow: Vec::new(),
            hint: None,
        };
        let mut saw_allow = false;
        for option in options {
            match keyword(option) {
                Some((Some("hint"), value)) if rule.hint.is_none() => {
                    rule.hint = Some(string(value).ok_or("hint must be a string literal")?.into());
                }
                Some((Some("allow_in"), value)) if !saw_allow => {
                    saw_allow = true;
                    let Some(Term::List(entries)) = term(value) else {
                        return Err("allow_in must be list(...)".into());
                    };
                    for entry in entries {
                        let Some(Term::Prefab(path)) = term(entry) else {
                            return Err("allow_in expects absolute type or proc paths".into());
                        };
                        let path = path.path.to_string();
                        let tree = proc.tree();
                        if !path.starts_with('/') {
                            return Err(format!("expected absolute allow_in path: {path}"));
                        }
                        if let Some((parent, name)) = path
                            .rsplit_once("/proc/")
                            .or_else(|| path.rsplit_once("/verb/"))
                        {
                            let parent = tree
                                .find(parent)
                                .ok_or_else(|| format!("unknown allow_in type {parent}"))?;
                            rule.allow.push(Allow::Proc(parent, name.into()));
                        } else {
                            let ty = tree
                                .find(&path)
                                .ok_or_else(|| format!("unknown allow_in type {path}"))?;
                            rule.allow.push(Allow::Type(ty));
                        }
                    }
                }
                _ => return Err("unknown or duplicate SET_VAR_RULE option".into()),
            }
        }
        Ok((member.to_owned(), rule))
    }

    pub fn check(
        &self,
        target: Target<'a, '_>,
        mutation: Mutation,
        proc: ProcRef<'a>,
    ) -> Vec<Issue> {
        let mut issues = Vec::new();
        let Some(rules) = self.0.get(target.member) else {
            return issues;
        };
        for rule in rules {
            let flags = match mutation {
                Mutation::Assign if !target.indexed => ASSIGN,
                Mutation::Assign | Mutation::List => LIST_MUTATE,
                Mutation::Alias => ASSIGN | LIST_MUTATE,
            };
            if rule.flags & flags == 0 || rule.allowed(proc) {
                continue;
            }
            let id = if let Some(owner) = target.owner {
                if owner.is_subtype_of(rule.owner.get()) {
                    match mutation {
                        Mutation::Alias => "var_alias_escape",
                        _ if flags == LIST_MUTATE => "var_list_mutate_forbidden",
                        _ => "var_assign_forbidden",
                    }
                } else if target.dynamic
                    && !owner.is_root()
                    && rule.owner.is_subtype_of(owner.get())
                {
                    "unverifiable_var_mutation"
                } else {
                    continue;
                }
            } else if target.dynamic {
                "unverifiable_var_mutation"
            } else {
                continue;
            };
            let field = format!("{}/{}", rule.owner.get().path, target.member);
            let mut message = match id {
                "var_alias_escape" => format!("list alias of {field} is mutated"),
                "unverifiable_var_mutation" => {
                    format!("receiver type is unknown for dynamic mutation of possible {field}")
                }
                _ => format!("mutation of {field} is forbidden"),
            };
            if let Some(hint) = &rule.hint {
                message.push_str(". ");
                message.push_str(hint);
            }
            issues.push((id, message));
        }
        issues
    }
}

impl Rule<'_> {
    fn allowed(&self, proc: ProcRef<'_>) -> bool {
        (self.flags & CONSTRUCTOR != 0
            && matches!(proc.name(), "New" | "Initialize")
            && proc.ty().is_subtype_of(self.owner.get()))
            || self.allow.iter().any(|entry| match entry {
                Allow::Type(ty) => proc.ty().is_subtype_of(ty.get()),
                Allow::Proc(ty, name) => proc.ty().index() == ty.index() && proc.name() == name,
            })
    }
}

fn decode_flags(value: &Expression) -> Result<u8, String> {
    if let Some(Term::Expr(inner)) = value.as_term() {
        return decode_flags(inner);
    }
    if let Expression::BinaryOp {
        op: BinaryOp::BitOr,
        lhs,
        rhs,
    } = value
    {
        return Ok(decode_flags(lhs)? | decode_flags(rhs)?);
    }
    match term(value) {
        Some(Term::Ident(name)) => match name.as_str() {
            "ASSIGN_NOT_ALLOWED" => Ok(ASSIGN),
            "LIST_MUTATE_NOT_ALLOWED" => Ok(LIST_MUTATE),
            "ALLOW_IN_CONSTRUCTOR" => Ok(CONSTRUCTOR),
            _ => Err(format!("unknown variable rule flag {name}")),
        },
        _ => Err("expected variable rule flags joined by |".into()),
    }
}

pub fn emit(out: &mut Reporter<'_>, loc: Location, issues: Vec<Issue>) {
    for (id, message) in issues {
        out.add(loc, id, message);
    }
}
