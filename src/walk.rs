use crate::analysis::{
    BLOCK_BYPASS, Issue, PARAM_SKIP, Reporter, VAR_BYPASS, keyword, string, term,
};
use crate::contracts::{Contracts, display};
use crate::vars::{Mutation, Rules, Target, emit};
use dreammaker::Location;
use dreammaker::ast::{
    AbsolutePath, AssignOp, Case, Expression, Follow, PathOp, Prefab, PropertyAccessKind, Spanned,
    Statement, Term, UnaryOp, VarStatement,
};
use dreammaker::objtree::{ProcRef, TypeRef};
use std::collections::HashMap;

pub fn analyze<'a>(
    proc: ProcRef<'a>,
    contracts: &Contracts<'a>,
    variables: &Rules<'a>,
    out: &mut Reporter<'_>,
) {
    let parameters = proc
        .parameters
        .iter()
        .map(|p| (p.name.as_str(), Local::new(p.var_type.type_path.clone())))
        .collect();
    let mut walker = Walker {
        proc,
        contracts,
        variables,
        out,
        scopes: vec![parameters],
        bypass: Vec::new(),
        skips: Vec::new(),
        depth: 0,
        uses_args: false,
    };
    walker.block(proc.code.as_deref().unwrap_or_default());
    if walker.uses_args && contracts.flags(proc).active() {
        walker.out.add(
            proc.location,
            "variadic",
            format!("{} uses the implicit args list", display(proc)),
        );
    }
}

struct Alias<'a> {
    owner: TypeRef<'a>,
    member: String,
}

struct Local<'a> {
    ty: AbsolutePath,
    inferred: bool,
    alias: Option<Alias<'a>>,
}

impl Local<'_> {
    fn new(ty: AbsolutePath) -> Self {
        Self {
            inferred: ty.is_empty(),
            ty,
            alias: None,
        }
    }
}

type SuppressedCall = (usize, Location, Vec<Issue>);

struct Walker<'a, 'b, 'c> {
    proc: ProcRef<'a>,
    contracts: &'b Contracts<'a>,
    variables: &'b Rules<'a>,
    out: &'b mut Reporter<'c>,
    scopes: Vec<HashMap<&'a str, Local<'a>>>,
    bypass: Vec<bool>,
    skips: Vec<Option<SuppressedCall>>,
    depth: usize,
    uses_args: bool,
}

impl<'a> Walker<'a, '_, '_> {
    fn local(&self, name: &str) -> Option<&Local<'a>> {
        self.scopes.iter().rev().find_map(|scope| scope.get(name))
    }

    fn declare(&mut self, name: &'a str, local: Local<'a>) {
        self.scopes.last_mut().unwrap().insert(name, local);
    }

    fn block(&mut self, block: &'a [Spanned<Statement>]) {
        self.scopes.push(HashMap::new());
        for stmt in block {
            self.statement(&stmt.elem);
        }
        self.scopes.pop();
    }

    fn statement(&mut self, statement: &'a Statement) {
        match statement {
            Statement::Expr(exp) | Statement::Throw(exp) | Statement::Del(exp) => self.expr(exp),
            Statement::Return(exp) | Statement::Crash(exp) => self.optional(exp.as_ref()),
            Statement::Setting { name, value, .. } => {
                if !crate::contracts::MARKERS.contains(&name.as_str())
                    && name.as_str() != crate::analysis::RETURN_TYPE
                {
                    self.expr(value);
                }
            }
            Statement::Var(var) => self.var(var),
            Statement::Vars(vars) => vars.iter().for_each(|var| self.var(var)),
            Statement::While { condition, block } => {
                self.expr(&condition.elem);
                self.block(block);
            }
            Statement::DoWhile { block, condition } => {
                self.block(block);
                self.expr(&condition.elem);
            }
            Statement::If { arms, else_arm } => {
                for (condition, block) in arms {
                    if matches!(term(&condition.elem), Some(Term::Call(name, args)) if name.as_str() == BLOCK_BYPASS && args.is_empty())
                    {
                        self.bypass.push(false);
                        self.block(block);
                        self.end_bypass(condition.location);
                    } else {
                        self.expr(&condition.elem);
                        self.block(block);
                    }
                }
                if let Some(block) = else_arm {
                    self.block(block);
                }
            }
            Statement::ForInfinite { block } | Statement::Label { block, .. } => self.block(block),
            Statement::ForLoop {
                init,
                test,
                inc,
                block,
            } => {
                self.scopes.push(HashMap::new());
                if let Some(init) = init {
                    self.statement(init);
                }
                self.optional(test.as_deref());
                self.block(block);
                if let Some(inc) = inc {
                    self.statement(inc);
                }
                self.scopes.pop();
            }
            Statement::ForList(for_list) => {
                self.optional(for_list.in_list.as_ref());
                let element_ty = for_list
                    .in_list
                    .as_ref()
                    .and_then(|e| list_element(&self.expression_type(e, None)?));
                self.scopes.push(HashMap::new());
                if let Some(var_type) = &for_list.var_type {
                    let mut ty = var_type.type_path.clone();
                    if ty.is_empty() {
                        ty = element_ty.unwrap_or_default();
                    }
                    self.declare(for_list.name.as_str(), Local::new(ty));
                } else if let Some(ty) = element_ty
                    && self
                        .local(for_list.name.as_str())
                        .is_some_and(|local| local.inferred)
                {
                    self.declare(for_list.name.as_str(), Local::new(ty));
                }
                self.block(&for_list.block);
                self.scopes.pop();
            }
            Statement::ForKeyValue(for_key) => {
                self.optional(for_key.in_list.as_ref());
                self.scopes.push(HashMap::new());
                if let Some(var_type) = &for_key.var_type {
                    self.declare(for_key.key.as_str(), Local::new(var_type.type_path.clone()));
                    let ty = for_key
                        .in_list
                        .as_ref()
                        .and_then(|e| list_element(&self.expression_type(e, None)?))
                        .unwrap_or_default();
                    self.declare(for_key.value.as_str(), Local::new(ty));
                }
                self.block(&for_key.block);
                self.scopes.pop();
            }
            Statement::ForRange(range) => {
                self.expr(&range.start);
                self.expr(&range.end);
                self.optional(range.step.as_ref());
                self.scopes.push(HashMap::new());
                if let Some(var_type) = &range.var_type {
                    self.declare(range.name.as_str(), Local::new(var_type.type_path.clone()));
                }
                self.block(&range.block);
                self.scopes.pop();
            }
            Statement::Spawn { delay, block } => {
                self.optional(delay.as_ref());
                self.block(block);
            }
            Statement::Switch {
                input,
                cases,
                default,
            } => {
                self.expr(input);
                for (conditions, block) in cases.iter() {
                    for condition in &conditions.elem {
                        match condition {
                            Case::Exact(e) => self.expr(e),
                            Case::Range(start, end) => {
                                self.expr(start);
                                self.expr(end);
                            }
                        }
                    }
                    self.block(block);
                }
                if let Some(default) = default {
                    self.block(default);
                }
            }
            Statement::TryCatch {
                try_block,
                catch_params,
                catch_block,
            } => {
                self.block(try_block);
                self.scopes.push(HashMap::new());
                for parameter in catch_params {
                    if let Some((name, path)) = parameter.as_slice().split_last() {
                        self.declare(name.as_str(), Local::new(path.iter().cloned().collect()));
                    }
                }
                self.block(catch_block);
                self.scopes.pop();
            }
            Statement::Continue(_) | Statement::Break(_) | Statement::Goto(_) => {}
        }
    }

    fn var(&mut self, var: &'a VarStatement) {
        let mut local = Local::new(var.var_type.type_path.clone());
        if let Some(value) = &var.value {
            self.expression(value, Some(&local.ty));
            if local.inferred {
                local.ty = self.expression_type(value, None).unwrap_or_default();
            }
            local.alias = self.alias_of(value, &local.ty);
        }
        self.declare(var.name.as_str(), local);
    }

    fn alias_of(&self, value: &'a Expression, local_ty: &AbsolutePath) -> Option<Alias<'a>> {
        if local_ty
            .as_slice()
            .first()
            .is_none_or(|i| i.as_str() != "list")
        {
            return None;
        }
        if let Some(target) = self.target(value)
            && !target.indexed
            && let Some(owner) = target.owner
        {
            return Some(Alias {
                owner,
                member: target.member.to_owned(),
            });
        }
        if let Some(Term::Ident(name)) = term(value)
            && let Some(alias) = self
                .local(name.as_str())
                .and_then(|local| local.alias.as_ref())
        {
            return Some(Alias {
                owner: alias.owner,
                member: alias.member.clone(),
            });
        }
        None
    }

    fn alias_target(&self, exp: &'a Expression) -> Option<(&Alias<'a>, bool)> {
        let Expression::Base { term, follow } = exp else {
            return None;
        };
        self.chain_alias(term, follow)
    }

    fn chain_alias(
        &self,
        base: &'a Spanned<Term>,
        follow: &'a [Spanned<Follow>],
    ) -> Option<(&Alias<'a>, bool)> {
        match follow {
            [] => match &base.elem {
                Term::Expr(expr) => self.alias_target(expr),
                Term::Ident(name) => self
                    .local(name.as_str())
                    .and_then(|local| local.alias.as_ref())
                    .map(|alias| (alias, false)),
                _ => None,
            },
            [rest @ .., last] if matches!(last.elem, Follow::Index(_, _)) => {
                self.chain_alias(base, rest).map(|(alias, _)| (alias, true))
            }
            _ => None,
        }
    }

    fn identifier_type(&self, name: &str, global: bool) -> Option<AbsolutePath> {
        let root = self.proc.tree().root();
        let src = self.proc.ty();
        if global {
            return self.member_type(root, name);
        }
        if let Some(local) = self.local(name) {
            return (!local.ty.is_empty()).then(|| local.ty.clone());
        }
        match name {
            "src" if !src.is_root() => Some(src.get().path.as_str().into()),
            "usr" => Some("/mob".into()),
            "world" => Some("/world".into()),
            _ => self
                .member_type(src, name)
                .or_else(|| self.member_type(root, name)),
        }
    }

    fn member_type(&self, ty: TypeRef<'a>, name: &str) -> Option<AbsolutePath> {
        let path = &ty.get_var_declaration(name)?.get().var_type.type_path;
        (!path.is_empty()).then(|| path.clone())
    }

    fn ty(&self, path: &AbsolutePath) -> Option<TypeRef<'a>> {
        if path.is_empty() {
            None
        } else {
            self.proc.tree().type_by_path(path.iter())
        }
    }

    fn type_of_prefab(&self, prefab: &Prefab) -> Option<TypeRef<'a>> {
        self.proc
            .ty()
            .navigate_path(prefab.path.as_slice())
            .map(|result| result.ty())
    }

    fn called(&self, owner: Option<TypeRef<'a>>, name: &str) -> Option<ProcRef<'a>> {
        owner?.get_proc(name)
    }

    fn bare_receiver(&self, name: &str) -> TypeRef<'a> {
        let ty = self.proc.ty();
        if !ty.is_root() && ty.get_proc(name).is_some() {
            ty
        } else {
            self.proc.tree().root()
        }
    }

    fn returned(&self, target: Option<ProcRef<'a>>, args: &[Expression]) -> Option<AbsolutePath> {
        let target = target?;
        let marker = self.contracts.return_type(target)?;
        if let Some(Term::Prefab(prefab)) = term(marker) {
            return self
                .type_of_prefab(prefab)
                .map(|ty| ty.get().path.as_str().into());
        }
        let Expression::Base { term, follow } = marker else {
            return None;
        };
        let Term::Ident(name) = &term.elem else {
            return None;
        };
        let index = target.parameters.iter().position(|p| p.name == *name)?;
        let arg = args
            .iter()
            .find_map(|arg| match keyword(arg) {
                Some((Some(key), value)) if key == name.as_str() => Some(value),
                _ => None,
            })
            .or_else(|| args.get(index).filter(|arg| keyword(arg).is_none()))?;
        if follow.is_empty() {
            let Some(Term::Prefab(prefab)) = crate::analysis::term(arg) else {
                return None;
            };
            return self
                .type_of_prefab(prefab)
                .map(|ty| ty.get().path.as_str().into());
        }
        let mut inferred = self.expression_type(arg, None)?;
        let (last, indices) = follow.split_last()?;
        if !matches!(&last.elem, Follow::Field(_, n) if n.as_str() == "type") {
            return None;
        }
        for index in indices {
            if !matches!(&index.elem, Follow::Index(_, e) if matches!(crate::analysis::term(e), Some(Term::Ident(n)) if n.as_str() == "_"))
            {
                return None;
            }
            inferred = list_element(&inferred)?;
        }
        Some(inferred)
    }

    fn expression_type(
        &self,
        expression: &Expression,
        context: Option<&AbsolutePath>,
    ) -> Option<AbsolutePath> {
        match expression {
            Expression::Base { term, follow } => self.chain_type(term, follow, context),
            Expression::TernaryOp { if_, else_, .. } => {
                let ty = self.expression_type(if_, context)?;
                (Some(&ty) == self.expression_type(else_, context).as_ref()).then_some(ty)
            }
            _ => None,
        }
    }

    fn chain_type(
        &self,
        term: &Spanned<Term>,
        follow: &[Spanned<Follow>],
        context: Option<&AbsolutePath>,
    ) -> Option<AbsolutePath> {
        let mut ty = self.term_type(&term.elem, context);
        let mut global = matches!(&term.elem, Term::Ident(name) if name.as_str() == "global" && self.local("global").is_none());
        for operation in follow {
            ty = self.follow_type(ty.as_ref(), &operation.elem, global);
            global = false;
        }
        ty
    }

    fn term_type(&self, node: &Term, context: Option<&AbsolutePath>) -> Option<AbsolutePath> {
        let proc = self.proc;
        match node {
            Term::Ident(name) => self.identifier_type(name.as_str(), false),
            Term::GlobalIdent(name) => self.identifier_type(name.as_str(), true),
            Term::Expr(expr) => self.expression_type(expr, context),
            Term::NewPrefab { prefab, .. } => self
                .type_of_prefab(prefab)
                .map(|ty| ty.get().path.as_str().into()),
            Term::NewImplicit { .. } => context.filter(|t| !t.is_empty()).cloned(),
            Term::Call(name, args) if matches!(name.as_str(), VAR_BYPASS | PARAM_SKIP) => {
                self.expression_type(args.first()?, context)
            }
            Term::Call(name, args) => self.returned(
                self.bare_receiver(name.as_str()).get_proc(name.as_str()),
                args,
            ),
            Term::GlobalCall(name, args) => {
                self.returned(proc.tree().root().get_proc(name.as_str()), args)
            }
            Term::SelfCall(args) => self.returned(proc.ty().get_proc(proc.name()), args),
            Term::ParentCall(args) => self.returned(proc.parent_proc(), args),
            Term::List(_) => Some("/list".into()),
            Term::Locate { args, .. } => match args.first().and_then(term) {
                Some(Term::Prefab(prefab)) => self
                    .type_of_prefab(prefab)
                    .map(|ty| ty.get().path.as_str().into()),
                _ => context.cloned(),
            },
            _ => None,
        }
    }

    fn follow_type(
        &self,
        path: Option<&AbsolutePath>,
        follow: &Follow,
        global: bool,
    ) -> Option<AbsolutePath> {
        let owner = if global {
            Some(self.proc.tree().root())
        } else {
            path.and_then(|p| self.ty(p))
        };
        match follow {
            Follow::Field(_, member) => self.member_type(owner?, member.as_str()),
            Follow::Index(_, _) => list_element(path?),
            Follow::Call(_, name, args) => self.returned(self.called(owner, name.as_str()), args),
            _ => None,
        }
    }

    fn target(&self, exp: &'a Expression) -> Option<Target<'a, 'a>> {
        let Expression::Base { term, follow } = exp else {
            return None;
        };
        self.chain_target(term, follow)
    }

    fn chain_target(
        &self,
        term: &'a Spanned<Term>,
        follow: &'a [Spanned<Follow>],
    ) -> Option<Target<'a, 'a>> {
        let Some((last, prefix)) = follow.split_last() else {
            return match &term.elem {
                Term::Expr(expr) => self.target(expr),
                Term::Ident(member) if self.local(member.as_str()).is_none() => Some(Target {
                    owner: Some(self.proc.ty()),
                    member: member.as_str(),
                    dynamic: false,
                    indexed: false,
                }),
                _ => None,
            };
        };
        match &last.elem {
            Follow::Field(kind, member) => Some(Target {
                owner: self
                    .chain_type(term, prefix, None)
                    .as_ref()
                    .and_then(|ty| self.ty(ty)),
                member: member.as_str(),
                dynamic: matches!(
                    kind,
                    PropertyAccessKind::Colon | PropertyAccessKind::SafeColon
                ),
                indexed: false,
            }),
            Follow::Index(_, index) => {
                let mut target = self.chain_target(term, prefix)?;
                if target.member == "vars" {
                    target.member = string(index)?;
                } else {
                    target.indexed = true;
                }
                Some(target)
            }
            _ => None,
        }
    }

    fn call_issues(&mut self, location: Location, relative_depth: usize, issues: Vec<Issue>) {
        if issues.is_empty() {
            return;
        }
        if let Some(frame) = self.skips.last_mut() {
            let depth = self.depth + relative_depth;
            if frame.as_ref().is_none_or(|(best, _, _)| depth < *best) {
                if let Some((_, former, issues)) = frame.replace((depth, location, issues)) {
                    emit(self.out, former, issues);
                }
                return;
            }
        }
        emit(self.out, location, issues);
    }

    fn mutation(&mut self, location: Location, issues: Vec<Issue>) {
        if issues.is_empty() {
            return;
        }
        if let Some(used) = self.bypass.last_mut() {
            *used = true;
        } else {
            emit(self.out, location, issues);
        }
    }

    fn end_bypass(&mut self, location: Location) {
        if self.bypass.pop() == Some(false) {
            self.out.add(
                location,
                "unused_var_bypass",
                "bypass does not cover a forbidden variable mutation",
            );
        }
    }

    fn optional(&mut self, expr: Option<&'a Expression>) {
        if let Some(expr) = expr {
            self.expr(expr);
        }
    }

    fn expr(&mut self, expr: &'a Expression) {
        self.expression(expr, None);
    }

    fn expression(&mut self, expr: &'a Expression, context: Option<&AbsolutePath>) {
        self.depth += 1;
        match expr {
            Expression::Base { term, follow } => self.chain(term, follow, context),
            Expression::BinaryOp { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            Expression::AssignOp { lhs, rhs, op } => self.assign(*op, lhs, rhs),
            Expression::TernaryOp { cond, if_, else_ } => {
                self.expr(cond);
                self.expression(if_, context);
                self.expression(else_, context);
            }
        }
        self.depth -= 1;
    }

    fn assign(&mut self, op: AssignOp, lhs: &'a Expression, rhs: &'a Expression) {
        let context = self.expression_type(lhs, None);
        let issues = if let Some(target) = self.target(lhs) {
            self.variables.check(target, Mutation::Assign, self.proc)
        } else if let Some((alias, indexed)) = self.alias_target(lhs)
            && (indexed
                || matches!(
                    op,
                    AssignOp::AddAssign
                        | AssignOp::SubAssign
                        | AssignOp::BitOrAssign
                        | AssignOp::BitAndAssign
                ))
        {
            self.variables.check(
                Target {
                    owner: Some(alias.owner),
                    member: &alias.member,
                    dynamic: false,
                    indexed,
                },
                Mutation::Alias,
                self.proc,
            )
        } else {
            Vec::new()
        };
        self.expr(lhs);
        let location = position(lhs);
        if let Some((wrapper, value)) = bypass_arg(rhs) {
            self.bypass.push(false);
            self.mutation(location, issues);
            self.expression(value, context.as_ref());
            self.end_bypass(wrapper);
        } else {
            self.mutation(location, issues);
            self.expression(rhs, context.as_ref());
        }
        if op == AssignOp::Assign
            && let Some(Term::Ident(name)) = term(lhs)
        {
            let inferred = self.expression_type(rhs, None).unwrap_or_default();
            let effective_ty = self
                .local(name.as_str())
                .map(|local| {
                    if local.inferred {
                        inferred.clone()
                    } else {
                        local.ty.clone()
                    }
                })
                .unwrap_or_default();
            let new_alias = self.alias_of(rhs, &effective_ty);
            if let Some(local) = self
                .scopes
                .iter_mut()
                .rev()
                .find_map(|scope| scope.get_mut(name.as_str()))
            {
                local.alias = new_alias;
                if local.inferred {
                    local.ty = inferred;
                }
            }
        }
    }

    fn chain(
        &mut self,
        base: &'a Spanned<Term>,
        follow: &'a [Spanned<Follow>],
        context: Option<&AbsolutePath>,
    ) {
        self.visit_term(base, context, follow.len());
        let mut ty = if follow.is_empty() {
            None
        } else {
            self.term_type(&base.elem, context)
        };
        let mut global = matches!(&base.elem, Term::Ident(n) if n.as_str() == "global" && self.local("global").is_none());
        for (i, op) in follow.iter().enumerate() {
            let depth = follow.len() - i - 1;
            let receiver = if global {
                Some(self.proc.tree().root())
            } else {
                ty.as_ref().and_then(|ty| self.ty(ty))
            };
            match &op.elem {
                Follow::Call(_, name, args) => {
                    let name = name.as_str();
                    let issues = self.contracts.call(receiver, name, args, global);
                    self.call_issues(base.location, depth, issues);
                    if matches!(
                        name,
                        "Add" | "Remove" | "Cut" | "Insert" | "Swap" | "Splice"
                    ) {
                        if let Some(target) = self.chain_target(base, &follow[..i]) {
                            self.mutation(
                                op.location,
                                self.variables.check(target, Mutation::List, self.proc),
                            );
                        } else if let Some((alias, indexed)) = self.chain_alias(base, &follow[..i])
                            && !indexed
                        {
                            self.mutation(
                                op.location,
                                self.variables.check(
                                    Target {
                                        owner: Some(alias.owner),
                                        member: &alias.member,
                                        dynamic: false,
                                        indexed: false,
                                    },
                                    Mutation::Alias,
                                    self.proc,
                                ),
                            );
                        }
                    }
                    self.args(args);
                }
                Follow::Index(_, expr) => self.expr(expr),
                Follow::Unary(
                    UnaryOp::PreIncr | UnaryOp::PostIncr | UnaryOp::PreDecr | UnaryOp::PostDecr,
                ) => {
                    if let Some(target) = self.chain_target(base, &follow[..i]) {
                        self.mutation(
                            op.location,
                            self.variables.check(target, Mutation::Assign, self.proc),
                        );
                    }
                }
                Follow::ProcReference(name) => {
                    let referenced = match &base.elem {
                        Term::Prefab(prefab) if i == 0 => self.type_of_prefab(prefab),
                        _ => receiver,
                    };
                    self.proc_reference(referenced, name.as_str(), op.location, depth);
                }
                _ => (),
            }
            ty = self.follow_type(ty.as_ref(), &op.elem, global);
            global = false;
        }
    }

    fn proc_reference(
        &mut self,
        owner: Option<TypeRef<'a>>,
        name: &str,
        loc: Location,
        depth: usize,
    ) {
        if self.contracts.possibly_marked(owner, name) {
            self.call_issues(
                loc,
                depth,
                vec![(
                    "proc_reference",
                    format!("reference to possibly marked proc `{name}` cannot be checked"),
                )],
            );
        }
    }

    fn args(&mut self, args: &'a [Expression]) {
        for arg in args {
            self.expr(keyword(arg).map_or(arg, |(_, value)| value));
        }
    }

    fn visit_term(
        &mut self,
        base: &'a Spanned<Term>,
        context: Option<&AbsolutePath>,
        depth: usize,
    ) {
        let loc = base.location;
        match &base.elem {
            Term::Ident(name) if name.as_str() == "args" && self.local("args").is_none() => {
                self.uses_args = true
            }
            Term::Expr(expr) => self.expression(expr, context),
            Term::Call(name, args) if name.as_str() == PARAM_SKIP && args.len() == 1 => {
                self.skips.push(None);
                self.expression(&args[0], context);
                if self.skips.pop().flatten().is_none() {
                    self.out.add(
                        loc,
                        "unused_skip",
                        "skip did not exempt a violating call or proc reference",
                    );
                }
            }
            Term::Call(name, args) if name.as_str() == VAR_BYPASS && args.len() == 1 => {
                self.bypass.push(false);
                self.expression(&args[0], context);
                self.end_bypass(loc);
            }
            Term::Call(name, args) if name.as_str() == "newlist" => {
                for arg in args {
                    if let Some(Term::Prefab(prefab)) = term(arg) {
                        let issues =
                            self.contracts
                                .call(self.type_of_prefab(prefab), "New", &[], true);
                        self.call_issues(position(arg), depth, issues);
                    } else if self.contracts.is_marked_name("New") {
                        let owner = self
                            .expression_type(arg, None)
                            .as_ref()
                            .and_then(|ty| self.ty(ty));
                        if self.contracts.possibly_marked(owner, "New") {
                            self.call_issues(
                                position(arg),
                                depth,
                                vec![(
                                    "unverifiable_call",
                                    "dynamic newlist() argument may construct a type with a marked New()"
                                        .into(),
                                )],
                            );
                        }
                    }
                }
                self.args(args);
            }
            Term::Call(name, args) | Term::GlobalCall(name, args) => {
                let global = matches!(base.elem, Term::GlobalCall(..));
                let owner = if global {
                    self.proc.tree().root()
                } else {
                    self.bare_receiver(name.as_str())
                };
                self.call_issues(
                    loc,
                    depth,
                    self.contracts
                        .call(Some(owner), name.as_str(), args, owner.is_root()),
                );
                self.args(args);
            }
            Term::SelfCall(args) => {
                let owner = self.proc.ty();
                self.call_issues(
                    loc,
                    depth,
                    self.contracts
                        .call(Some(owner), self.proc.name(), args, owner.is_root()),
                );
                self.args(args);
            }
            Term::ParentCall(args) => {
                if !args.is_empty()
                    && let Some(parent) = self.proc.parent_proc()
                {
                    self.call_issues(loc, depth, self.contracts.check(parent, args));
                }
                self.args(args);
            }
            Term::NewPrefab { prefab, args } => {
                let args = args.as_deref().unwrap_or_default();
                self.call_issues(
                    loc,
                    depth,
                    self.contracts
                        .call(self.type_of_prefab(prefab), "New", args, true),
                );
                self.args(args);
                self.prefab_values(prefab);
            }
            Term::NewImplicit { args } => {
                let args = args.as_deref().unwrap_or_default();
                self.call_issues(
                    loc,
                    depth,
                    self.contracts
                        .call(context.and_then(|ty| self.ty(ty)), "New", args, true),
                );
                self.args(args);
            }
            Term::NewMiniExpr { expr, args } => {
                let mut path = if expr.ident.as_str() == "type" && self.local("type").is_none() {
                    Some(self.proc.ty().get().path.as_str().into())
                } else {
                    self.identifier_type(expr.ident.as_str(), false)
                };
                for field in &expr.fields {
                    if field.ident.as_str() != "type" {
                        path = path
                            .as_ref()
                            .and_then(|ty| self.ty(ty))
                            .and_then(|t| self.member_type(t, field.ident.as_str()));
                    }
                }
                if let Some(owner) = path.as_ref().or(context).and_then(|ty| self.ty(ty))
                    && self.contracts.possibly_marked(Some(owner), "New")
                {
                    self.call_issues(
                        loc,
                        depth,
                        vec![(
                            "unverifiable_call",
                            format!(
                                "dynamic new may construct {} or a subtype with a marked New()",
                                owner.get().path
                            ),
                        )],
                    );
                }
                self.args(args.as_deref().unwrap_or_default());
            }
            Term::Prefab(prefab) => {
                let parts = prefab.path.as_slice();
                if let [ty @ .., (_, kind), (_, name)] = parts
                    && matches!(kind.as_str(), "proc" | "verb")
                    && self.contracts.is_marked_name(name.as_str())
                {
                    let owner = if ty.is_empty() {
                        if parts[0].0 == PathOp::Slash {
                            Some(self.proc.tree().root())
                        } else {
                            Some(self.proc.ty())
                        }
                    } else {
                        self.proc.ty().navigate_path(ty).map(|result| result.ty())
                    };
                    self.proc_reference(owner, name.as_str(), loc, depth);
                }
                self.prefab_values(prefab);
            }
            Term::DynamicCall(target, args) => {
                if let Some(name) = target.last().and_then(string) {
                    let ty = if target.len() == 1 {
                        Some(self.proc.tree().root())
                    } else {
                        self.expression_type(&target[0], None)
                            .as_ref()
                            .and_then(|path| self.ty(path))
                    };
                    if self.contracts.possibly_marked(ty, name) {
                        self.call_issues(
                            loc,
                            depth,
                            vec![(
                                "dynamic_call",
                                format!("call() may reach marked proc `{name}`"),
                            )],
                        );
                    }
                }
                self.args(target);
                self.args(args);
            }
            Term::List(entries) => {
                for entry in entries {
                    if let Expression::AssignOp {
                        op: AssignOp::Assign,
                        lhs,
                        rhs,
                    } = entry
                    {
                        // Associative list keys are expressions, rather than
                        // keyword names or writes to the left operand.
                        self.expr(lhs);
                        self.expr(rhs);
                    } else {
                        self.expr(entry);
                    }
                }
            }
            Term::Input { args, in_list, .. } | Term::Locate { args, in_list } => {
                self.args(args);
                self.optional(in_list.as_deref());
            }
            Term::Pick(args) => {
                for (weight, value) in args.iter() {
                    self.optional(weight.as_ref());
                    self.expr(value);
                }
            }
            Term::ExternalCall {
                library,
                function,
                args,
            } => {
                self.optional(library.as_deref());
                self.expr(function);
                self.args(args);
            }
            Term::InterpString(_, parts) => {
                for (value, _) in parts.iter() {
                    self.optional(value.as_ref());
                }
            }
            _ => (),
        }
    }

    fn prefab_values(&mut self, prefab: &'a Prefab) {
        for (_, expr) in prefab.vars.iter() {
            self.expr(expr);
        }
    }
}

fn list_element(path: &AbsolutePath) -> Option<AbsolutePath> {
    let (head, tail) = path.as_slice().split_first()?;
    (head.as_str() == "list" && !tail.is_empty()).then(|| tail.iter().cloned().collect())
}

fn position(e: &Expression) -> Location {
    match e {
        Expression::Base { term, .. } => term.location,
        Expression::BinaryOp { lhs, .. } | Expression::AssignOp { lhs, .. } => position(lhs),
        Expression::TernaryOp { cond, .. } => position(cond),
    }
}

fn bypass_arg(e: &Expression) -> Option<(Location, &Expression)> {
    let Expression::Base { term, follow } = e else {
        return None;
    };
    if !follow.is_empty() {
        return None;
    }
    match &term.elem {
        Term::Call(name, args) if name.as_str() == VAR_BYPASS && args.len() == 1 => {
            Some((term.location, &args[0]))
        }
        Term::Expr(inner) => bypass_arg(inner),
        _ => None,
    }
}
