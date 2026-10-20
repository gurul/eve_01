//! Python's `ast.parse` and `ast.walk`, as much of them as `workbench_slugs` reads.
//!
//! The script is parsed with Ruff's parser (RustPython's fork), pinned to the Python 3.13 grammar the daemon runs
//! on, and its tree is re-shaped into CPython's: one [`PyNode`] per CPython `ast.AST` node that can hold another
//! node, children in the order `ast.iter_child_nodes` yields them (each class's `_fields` order). [`walk`] is then
//! `ast.walk`, breadth first, so the slugs a script calls come out in the order the Python gate lists them.
//!
//! Leaf nodes (`Constant`, the `Load`/`Store` contexts, operators) are left out: a node with no children changes
//! no other node's place in a breadth-first walk, and the gate never looks at them.

use std::collections::VecDeque;

use ruff_python_ast as ast;
use ruff_python_ast::{Expr, Pattern, Stmt};
use ruff_python_parser::{parse, Mode, ParseOptions};

/// The first argument a call is given, as the gate reads it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FirstArg {
    /// No positional argument and none of the slug keywords.
    Missing,
    /// A `str` constant (implicitly concatenated literals included, as CPython folds them).
    Str(String),
    /// Anything else: a name, an f-string, bytes, a number, a starred value...
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Kind {
    /// `ast.Import`: each alias's dotted name.
    Import(Vec<String>),
    /// `ast.ImportFrom`: its module (None for `from . import x`) and level.
    ImportFrom { module: Option<String>, level: u32 },
    /// `ast.Name`.
    Name(String),
    /// `ast.Attribute`: the attribute's name.
    Attribute(String),
    /// `ast.keyword`: its `arg` (None for `**kwargs`).
    Keyword(Option<String>),
    /// `ast.alias`: its `name`.
    Alias(String),
    /// `ast.Call`: the called name when `func` is a plain `ast.Name`, and the first argument the helper's
    /// slug would be read from (`args[0]`, else the first `tool_slug`/`slug`/`action` keyword).
    Call { func_name: Option<String>, first: FirstArg },
    /// Every other node that can hold children.
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PyNode {
    pub kind: Kind,
    pub children: Vec<PyNode>,
}

impl PyNode {
    fn new(kind: Kind, children: Vec<PyNode>) -> Self {
        PyNode { kind, children }
    }

    fn other(children: Vec<PyNode>) -> Self {
        PyNode::new(Kind::Other, children)
    }
}

/// `ast.parse(code)` as a [`PyNode`] tree, or None where Python raises SyntaxError or ValueError (a null byte,
/// syntax newer or older than Python 3.13's).
pub(crate) fn parse_module(code: &str) -> Option<PyNode> {
    if code.contains('\0') {
        return None; // "source code string cannot contain null bytes"
    }
    let options = ParseOptions::from(Mode::Module).with_target_version(ast::PythonVersion::PY313);
    let parsed = parse(code, options).ok()?;
    if !parsed.errors().is_empty() || !parsed.unsupported_syntax_errors().is_empty() {
        return None;
    }
    match parsed.into_syntax() {
        ast::Mod::Module(module) => Some(PyNode::other(stmts(&module.body))),
        ast::Mod::Expression(_) => None,
    }
}

/// `ast.walk(tree)`: every node, breadth first from the root (CPython pops the left of a deque and extends it
/// with each node's children).
pub(crate) fn walk(root: &PyNode) -> Vec<&PyNode> {
    let mut out = Vec::new();
    let mut todo: VecDeque<&PyNode> = VecDeque::from([root]);
    while let Some(node) = todo.pop_front() {
        todo.extend(node.children.iter());
        out.push(node);
    }
    out
}

fn stmts(body: &[Stmt]) -> Vec<PyNode> {
    body.iter().map(stmt).collect()
}

fn exprs<'a>(items: impl IntoIterator<Item = &'a Expr>) -> Vec<PyNode> {
    items.into_iter().map(expr).collect()
}

fn opt(e: Option<&Expr>) -> Vec<PyNode> {
    e.map(expr).into_iter().collect()
}

fn ident(id: &ast::Identifier) -> String {
    id.id.as_str().to_string()
}

fn type_params(tp: Option<&ast::TypeParams>) -> Vec<PyNode> {
    let Some(tp) = tp else { return Vec::new() };
    tp.type_params
        .iter()
        .map(|p| match p {
            // TypeVar(name, bound, default_value)
            ast::TypeParam::TypeVar(v) => {
                let mut c = opt(v.bound.as_deref());
                c.extend(opt(v.default.as_deref()));
                PyNode::other(c)
            }
            // ParamSpec(name, default_value), TypeVarTuple(name, default_value)
            ast::TypeParam::ParamSpec(v) => PyNode::other(opt(v.default.as_deref())),
            ast::TypeParam::TypeVarTuple(v) => PyNode::other(opt(v.default.as_deref())),
        })
        .collect()
}

/// `ast.arg(arg, annotation, type_comment)`.
fn arg(p: &ast::Parameter) -> PyNode {
    PyNode::other(opt(p.annotation.as_deref()))
}

/// `ast.arguments(posonlyargs, args, vararg, kwonlyargs, kw_defaults, kwarg, defaults)`.
fn arguments(p: Option<&ast::Parameters>) -> PyNode {
    let Some(p) = p else { return PyNode::other(Vec::new()) };
    let mut c: Vec<PyNode> = Vec::new();
    c.extend(p.posonlyargs.iter().map(|a| arg(&a.parameter)));
    c.extend(p.args.iter().map(|a| arg(&a.parameter)));
    if let Some(v) = &p.vararg {
        c.push(arg(v));
    }
    c.extend(p.kwonlyargs.iter().map(|a| arg(&a.parameter)));
    // kw_defaults holds None for a keyword-only argument without a default: not a node
    c.extend(p.kwonlyargs.iter().filter_map(|a| a.default.as_deref()).map(expr));
    if let Some(k) = &p.kwarg {
        c.push(arg(k));
    }
    c.extend(p.posonlyargs.iter().chain(p.args.iter()).filter_map(|a| a.default.as_deref()).map(expr));
    PyNode::other(c)
}

/// `ast.keyword(arg, value)`.
fn keyword(k: &ast::Keyword) -> PyNode {
    PyNode::new(Kind::Keyword(k.arg.as_ref().map(ident)), vec![expr(&k.value)])
}

/// `ast.alias(name, asname)`.
fn alias(a: &ast::Alias) -> PyNode {
    PyNode::new(Kind::Alias(ident(&a.name)), Vec::new())
}

/// `ast.comprehension(target, iter, ifs, is_async)`.
fn comprehension(c: &ast::Comprehension) -> PyNode {
    let mut out = vec![expr(&c.target), expr(&c.iter)];
    out.extend(exprs(&c.ifs));
    PyNode::other(out)
}

/// The `If` CPython makes of `if/elif/else`: each `elif` is an `If` alone in the previous one's `orelse`.
fn if_chain(test: &Expr, body: &[Stmt], clauses: &[ast::ElifElseClause]) -> PyNode {
    let mut c = vec![expr(test)];
    c.extend(stmts(body));
    if let Some((first, rest)) = clauses.split_first() {
        match &first.test {
            Some(t) => c.push(if_chain(t, &first.body, rest)),
            None => c.extend(stmts(&first.body)),
        }
    }
    PyNode::other(c)
}

fn stmt(s: &Stmt) -> PyNode {
    match s {
        // FunctionDef(name, args, body, decorator_list, returns, type_comment, type_params)
        Stmt::FunctionDef(f) => {
            let mut c = vec![arguments(Some(&f.parameters))];
            c.extend(stmts(&f.body));
            c.extend(f.decorator_list.iter().map(|d| expr(&d.expression)));
            c.extend(opt(f.returns.as_deref()));
            c.extend(type_params(f.type_params.as_deref()));
            PyNode::other(c)
        }
        // ClassDef(name, bases, keywords, body, decorator_list, type_params)
        Stmt::ClassDef(cd) => {
            let mut c = Vec::new();
            if let Some(a) = &cd.arguments {
                c.extend(exprs(a.args.iter()));
                c.extend(a.keywords.iter().map(keyword));
            }
            c.extend(stmts(&cd.body));
            c.extend(cd.decorator_list.iter().map(|d| expr(&d.expression)));
            c.extend(type_params(cd.type_params.as_deref()));
            PyNode::other(c)
        }
        Stmt::Return(r) => PyNode::other(opt(r.value.as_deref())),
        Stmt::Delete(d) => PyNode::other(exprs(&d.targets)),
        // TypeAlias(name, type_params, value)
        Stmt::TypeAlias(t) => {
            let mut c = vec![expr(&t.name)];
            c.extend(type_params(t.type_params.as_deref()));
            c.push(expr(&t.value));
            PyNode::other(c)
        }
        // Assign(targets, value, type_comment)
        Stmt::Assign(a) => {
            let mut c = exprs(&a.targets);
            c.push(expr(&a.value));
            PyNode::other(c)
        }
        // AugAssign(target, op, value)
        Stmt::AugAssign(a) => PyNode::other(vec![expr(&a.target), expr(&a.value)]),
        // AnnAssign(target, annotation, value, simple)
        Stmt::AnnAssign(a) => {
            let mut c = vec![expr(&a.target), expr(&a.annotation)];
            c.extend(opt(a.value.as_deref()));
            PyNode::other(c)
        }
        // For(target, iter, body, orelse, type_comment)
        Stmt::For(f) => {
            let mut c = vec![expr(&f.target), expr(&f.iter)];
            c.extend(stmts(&f.body));
            c.extend(stmts(&f.orelse));
            PyNode::other(c)
        }
        // While(test, body, orelse)
        Stmt::While(w) => {
            let mut c = vec![expr(&w.test)];
            c.extend(stmts(&w.body));
            c.extend(stmts(&w.orelse));
            PyNode::other(c)
        }
        Stmt::If(i) => if_chain(&i.test, &i.body, &i.elif_else_clauses),
        // With(items, body, type_comment); withitem(context_expr, optional_vars)
        Stmt::With(w) => {
            let mut c: Vec<PyNode> = w
                .items
                .iter()
                .map(|item| {
                    let mut ic = vec![expr(&item.context_expr)];
                    ic.extend(opt(item.optional_vars.as_deref()));
                    PyNode::other(ic)
                })
                .collect();
            c.extend(stmts(&w.body));
            PyNode::other(c)
        }
        // Match(subject, cases); match_case(pattern, guard, body)
        Stmt::Match(m) => {
            let mut c = vec![expr(&m.subject)];
            c.extend(m.cases.iter().map(|case| {
                let mut cc = vec![pattern(&case.pattern)];
                cc.extend(opt(case.guard.as_deref()));
                cc.extend(stmts(&case.body));
                PyNode::other(cc)
            }));
            PyNode::other(c)
        }
        // Raise(exc, cause)
        Stmt::Raise(r) => {
            let mut c = opt(r.exc.as_deref());
            c.extend(opt(r.cause.as_deref()));
            PyNode::other(c)
        }
        // Try / TryStar(body, handlers, orelse, finalbody); ExceptHandler(type, name, body)
        Stmt::Try(t) => {
            let mut c = stmts(&t.body);
            c.extend(t.handlers.iter().map(|h| match h {
                ast::ExceptHandler::ExceptHandler(h) => {
                    let mut hc = opt(h.type_.as_deref());
                    hc.extend(stmts(&h.body));
                    PyNode::other(hc)
                }
            }));
            c.extend(stmts(&t.orelse));
            c.extend(stmts(&t.finalbody));
            PyNode::other(c)
        }
        // Assert(test, msg)
        Stmt::Assert(a) => {
            let mut c = vec![expr(&a.test)];
            c.extend(opt(a.msg.as_deref()));
            PyNode::other(c)
        }
        Stmt::Import(i) => PyNode::new(
            Kind::Import(i.names.iter().map(|a| ident(&a.name)).collect()),
            i.names.iter().map(alias).collect(),
        ),
        Stmt::ImportFrom(i) => PyNode::new(
            Kind::ImportFrom { module: i.module.as_ref().map(ident), level: i.level },
            i.names.iter().map(alias).collect(),
        ),
        Stmt::Expr(e) => PyNode::other(vec![expr(&e.value)]),
        Stmt::Global(_)
        | Stmt::Nonlocal(_)
        | Stmt::Pass(_)
        | Stmt::Break(_)
        | Stmt::Continue(_)
        | Stmt::IpyEscapeCommand(_) => PyNode::other(Vec::new()),
    }
}

/// The `FormattedValue(value, conversion, format_spec)` nodes of an f-string's elements, in order (the literal
/// pieces are `Constant` leaves).
fn interpolations(elements: &ast::InterpolatedStringElements) -> Vec<PyNode> {
    elements
        .iter()
        .filter_map(|el| match el {
            ast::InterpolatedStringElement::Interpolation(i) => {
                let mut c = vec![expr(&i.expression)];
                if let Some(spec) = &i.format_spec {
                    // format_spec is a JoinedStr of its own
                    c.push(PyNode::other(interpolations(&spec.elements)));
                }
                Some(PyNode::other(c))
            }
            ast::InterpolatedStringElement::Literal(_) => None,
        })
        .collect()
}

fn pattern(p: &Pattern) -> PyNode {
    match p {
        // MatchValue(value)
        Pattern::MatchValue(v) => PyNode::other(vec![expr(&v.value)]),
        Pattern::MatchSingleton(_) | Pattern::MatchStar(_) => PyNode::other(Vec::new()),
        // MatchSequence(patterns), MatchOr(patterns)
        Pattern::MatchSequence(s) => PyNode::other(s.patterns.iter().map(pattern).collect()),
        Pattern::MatchOr(o) => PyNode::other(o.patterns.iter().map(pattern).collect()),
        // MatchMapping(keys, patterns, rest)
        Pattern::MatchMapping(m) => {
            let mut c = exprs(m.keys.iter());
            c.extend(m.patterns.iter().map(pattern));
            PyNode::other(c)
        }
        // MatchClass(cls, patterns, kwd_attrs, kwd_patterns)
        Pattern::MatchClass(m) => {
            let mut c = vec![expr(&m.cls)];
            c.extend(m.arguments.patterns.iter().map(pattern));
            c.extend(m.arguments.keywords.iter().map(|k| pattern(&k.pattern)));
            PyNode::other(c)
        }
        // MatchAs(pattern, name)
        Pattern::MatchAs(a) => PyNode::other(a.pattern.as_deref().map(pattern).into_iter().collect()),
    }
}

fn first_arg(call: &ast::ExprCall) -> FirstArg {
    let value = match call.arguments.args.first() {
        Some(a) => Some(a),
        None => call
            .arguments
            .keywords
            .iter()
            .find(|k| k.arg.as_ref().is_some_and(|a| matches!(a.id.as_str(), "tool_slug" | "slug" | "action")))
            .map(|k| &k.value),
    };
    match value {
        None => FirstArg::Missing,
        Some(Expr::StringLiteral(s)) => FirstArg::Str(s.value.to_str().to_string()),
        Some(_) => FirstArg::Other,
    }
}

fn expr(e: &Expr) -> PyNode {
    match e {
        // BoolOp(op, values)
        Expr::BoolOp(b) => PyNode::other(exprs(&b.values)),
        // NamedExpr(target, value)
        Expr::Named(n) => PyNode::other(vec![expr(&n.target), expr(&n.value)]),
        // BinOp(left, op, right)
        Expr::BinOp(b) => PyNode::other(vec![expr(&b.left), expr(&b.right)]),
        // UnaryOp(op, operand)
        Expr::UnaryOp(u) => PyNode::other(vec![expr(&u.operand)]),
        // Lambda(args, body)
        Expr::Lambda(l) => PyNode::other(vec![arguments(l.parameters.as_deref()), expr(&l.body)]),
        // IfExp(test, body, orelse)
        Expr::If(i) => PyNode::other(vec![expr(&i.test), expr(&i.body), expr(&i.orelse)]),
        // Dict(keys, values): every key first (None for a ** item is not a node), then every value
        Expr::Dict(d) => {
            let mut c: Vec<PyNode> = d.items.iter().filter_map(|i| i.key.as_ref()).map(expr).collect();
            c.extend(d.items.iter().map(|i| expr(&i.value)));
            PyNode::other(c)
        }
        Expr::Set(s) => PyNode::other(exprs(&s.elts)),
        // ListComp / SetComp / GeneratorExp(elt, generators)
        Expr::ListComp(l) => {
            let mut c = vec![expr(&l.elt)];
            c.extend(l.generators.iter().map(comprehension));
            PyNode::other(c)
        }
        Expr::SetComp(l) => {
            let mut c = vec![expr(&l.elt)];
            c.extend(l.generators.iter().map(comprehension));
            PyNode::other(c)
        }
        Expr::Generator(l) => {
            let mut c = vec![expr(&l.elt)];
            c.extend(l.generators.iter().map(comprehension));
            PyNode::other(c)
        }
        // DictComp(key, value, generators)
        Expr::DictComp(d) => {
            let mut c = opt(d.key.as_deref());
            c.push(expr(&d.value));
            c.extend(d.generators.iter().map(comprehension));
            PyNode::other(c)
        }
        Expr::Await(a) => PyNode::other(vec![expr(&a.value)]),
        Expr::Yield(y) => PyNode::other(opt(y.value.as_deref())),
        Expr::YieldFrom(y) => PyNode::other(vec![expr(&y.value)]),
        // Compare(left, ops, comparators)
        Expr::Compare(c) => {
            let mut out = vec![expr(&c.left)];
            out.extend(exprs(c.comparators.iter()));
            PyNode::other(out)
        }
        // Call(func, args, keywords)
        Expr::Call(call) => {
            let func_name = match call.func.as_ref() {
                Expr::Name(n) => Some(n.id.as_str().to_string()),
                _ => None,
            };
            let mut c = vec![expr(&call.func)];
            c.extend(exprs(call.arguments.args.iter()));
            c.extend(call.arguments.keywords.iter().map(keyword));
            PyNode::new(Kind::Call { func_name, first: first_arg(call) }, c)
        }
        // JoinedStr(values): implicitly concatenated pieces are one JoinedStr in CPython
        Expr::FString(f) => PyNode::other(
            f.value
                .iter()
                .flat_map(|part| match part {
                    ast::FStringPart::FString(fs) => interpolations(&fs.elements),
                    ast::FStringPart::Literal(_) => Vec::new(),
                })
                .collect(),
        ),
        // TemplateStr(values) / Interpolation (3.14; refused by the 3.13 grammar before it gets here)
        Expr::TString(t) => {
            PyNode::other(t.value.iter().flat_map(|ts| interpolations(&ts.elements)).collect())
        }
        Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::NumberLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NoneLiteral(_)
        | Expr::EllipsisLiteral(_)
        | Expr::Constant(_)
        | Expr::IpyEscapeCommand(_) => PyNode::other(Vec::new()),
        // Attribute(value, attr, ctx)
        Expr::Attribute(a) => PyNode::new(Kind::Attribute(ident(&a.attr)), vec![expr(&a.value)]),
        // Subscript(value, slice, ctx)
        Expr::Subscript(s) => PyNode::other(vec![expr(&s.value), expr(&s.slice)]),
        Expr::Starred(s) => PyNode::other(vec![expr(&s.value)]),
        Expr::Name(n) => PyNode::new(Kind::Name(n.id.as_str().to_string()), Vec::new()),
        Expr::List(l) => PyNode::other(exprs(&l.elts)),
        Expr::Tuple(t) => PyNode::other(exprs(&t.elts)),
        // Slice(lower, upper, step)
        Expr::Slice(s) => {
            let mut c = opt(s.lower.as_deref());
            c.extend(opt(s.upper.as_deref()));
            c.extend(opt(s.step.as_deref()));
            PyNode::other(c)
        }
    }
}
