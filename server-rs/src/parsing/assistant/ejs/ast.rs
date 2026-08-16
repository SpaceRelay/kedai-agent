// 语法树:EJS JS 子集 AST 定义(Stmt/Expr/UnOp/BinOp/AssignOp/UpdateOp/AssignTarget/FnBody)。
// 由 parser.rs 构建,eval.rs/exec.rs 消费;Expr 引用 value.rs 的 JsValue(字面量)。
use super::value::JsValue;

#[derive(Debug, Clone)]
pub(super) enum Stmt {
    Expr(Expr),
    /// var/let/const 声明(统一按 var 语义:当前作用域)
    Var(Vec<(String, Option<Expr>)>),
    If {
        cond: Expr,
        then: Box<Stmt>,
        els: Option<Box<Stmt>>,
    },
    Block(Vec<Stmt>),
    For {
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        update: Option<Expr>,
        body: Box<Stmt>,
    },
    ForOf {
        name: String,
        iter: Expr,
        body: Box<Stmt>,
    },
    While {
        cond: Expr,
        body: Box<Stmt>,
    },
    Function {
        name: String,
        params: Vec<FuncParam>,
        body: Vec<Stmt>,
    },
    Return(Option<Expr>),
    Break,
    Continue,
    Empty,
}

/// 函数参数(支持 JS 默认参数语法 `name = expr`,expr 在调用时求值)
#[derive(Debug, Clone)]
pub(super) struct FuncParam {
    pub(super) name: String,
    pub(super) default: Option<Expr>,
}

#[derive(Debug, Clone)]
pub(super) enum Expr {
    Literal(JsValue),
    Ident(String),
    Member {
        obj: Box<Expr>,
        prop: String,
    },
    Index {
        obj: Box<Expr>,
        idx: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    Unary {
        op: UnOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Ternary {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },
    Assign {
        target: AssignTarget,
        op: AssignOp,
        value: Box<Expr>,
    },
    Update {
        target: AssignTarget,
        op: UpdateOp,
        prefix: bool,
    },
    Array(Vec<Expr>),
    Object(Vec<(String, Expr)>),
    Func {
        params: Vec<FuncParam>,
        body: FnBody,
    },
    Template(Vec<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum UnOp {
    Not,
    Neg,
    Pos,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum BinOp {
    Or,
    And,
    Eq,
    NotEq,
    StrictEq,
    StrictNotEq,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum AssignOp {
    Set,
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum UpdateOp {
    Inc,
    Dec,
}

#[derive(Debug, Clone)]
pub(super) enum AssignTarget {
    Ident(String),
    Member(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone)]
pub(super) enum FnBody {
    Block(Vec<Stmt>),
    Expr(Box<Expr>),
}
