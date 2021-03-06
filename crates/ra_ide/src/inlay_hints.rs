//! FIXME: write short doc here

use hir::{Adt, HirDisplay, Semantics, Type};
use ra_ide_db::RootDatabase;
use ra_prof::profile;
use ra_syntax::{
    ast::{self, ArgListOwner, AstNode, TypeAscriptionOwner},
    match_ast, Direction, NodeOrToken, SmolStr, SyntaxKind, TextRange,
};

use crate::{FileId, FunctionSignature};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlayHintsOptions {
    pub type_hints: bool,
    pub parameter_hints: bool,
    pub chaining_hints: bool,
    pub max_length: Option<usize>,
}

impl Default for InlayHintsOptions {
    fn default() -> Self {
        Self { type_hints: true, parameter_hints: true, chaining_hints: true, max_length: None }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InlayKind {
    TypeHint,
    ParameterHint,
    ChainingHint,
}

#[derive(Debug)]
pub struct InlayHint {
    pub range: TextRange,
    pub kind: InlayKind,
    pub label: SmolStr,
}

pub(crate) fn inlay_hints(
    db: &RootDatabase,
    file_id: FileId,
    options: &InlayHintsOptions,
) -> Vec<InlayHint> {
    let _p = profile("inlay_hints");
    let sema = Semantics::new(db);
    let file = sema.parse(file_id);

    let mut res = Vec::new();
    for node in file.syntax().descendants() {
        if let Some(expr) = ast::Expr::cast(node.clone()) {
            get_chaining_hints(&mut res, &sema, options, expr);
        }

        match_ast! {
            match node {
                ast::CallExpr(it) => { get_param_name_hints(&mut res, &sema, options, ast::Expr::from(it)); },
                ast::MethodCallExpr(it) => { get_param_name_hints(&mut res, &sema, options, ast::Expr::from(it)); },
                ast::BindPat(it) => { get_bind_pat_hints(&mut res, &sema, options, it); },
                _ => (),
            }
        }
    }
    res
}

fn get_chaining_hints(
    acc: &mut Vec<InlayHint>,
    sema: &Semantics<RootDatabase>,
    options: &InlayHintsOptions,
    expr: ast::Expr,
) -> Option<()> {
    if !options.chaining_hints {
        return None;
    }

    let ty = sema.type_of_expr(&expr)?;
    if ty.is_unknown() {
        return None;
    }

    let mut tokens = expr
        .syntax()
        .siblings_with_tokens(Direction::Next)
        .filter_map(NodeOrToken::into_token)
        .filter(|t| match t.kind() {
            SyntaxKind::WHITESPACE if !t.text().contains('\n') => false,
            SyntaxKind::COMMENT => false,
            _ => true,
        });

    // Chaining can be defined as an expression whose next sibling tokens are newline and dot
    // Ignoring extra whitespace and comments
    let next = tokens.next()?.kind();
    let next_next = tokens.next()?.kind();
    if next == SyntaxKind::WHITESPACE && next_next == SyntaxKind::DOT {
        let label = ty.display_truncated(sema.db, options.max_length).to_string();
        acc.push(InlayHint {
            range: expr.syntax().text_range(),
            kind: InlayKind::ChainingHint,
            label: label.into(),
        });
    }
    Some(())
}

fn get_param_name_hints(
    acc: &mut Vec<InlayHint>,
    sema: &Semantics<RootDatabase>,
    options: &InlayHintsOptions,
    expr: ast::Expr,
) -> Option<()> {
    if !options.parameter_hints {
        return None;
    }

    let args = match &expr {
        ast::Expr::CallExpr(expr) => expr.arg_list()?.args(),
        ast::Expr::MethodCallExpr(expr) => expr.arg_list()?.args(),
        _ => return None,
    };
    let args_count = args.clone().count();

    let fn_signature = get_fn_signature(sema, &expr)?;
    let n_params_to_skip =
        if fn_signature.has_self_param && fn_signature.parameter_names.len() > args_count {
            1
        } else {
            0
        };
    let hints = fn_signature
        .parameter_names
        .iter()
        .skip(n_params_to_skip)
        .zip(args)
        .filter(|(param, arg)| should_show_param_hint(&fn_signature, param, &arg))
        .map(|(param_name, arg)| InlayHint {
            range: arg.syntax().text_range(),
            kind: InlayKind::ParameterHint,
            label: param_name.into(),
        });

    acc.extend(hints);
    Some(())
}

fn get_bind_pat_hints(
    acc: &mut Vec<InlayHint>,
    sema: &Semantics<RootDatabase>,
    options: &InlayHintsOptions,
    pat: ast::BindPat,
) -> Option<()> {
    if !options.type_hints {
        return None;
    }

    let ty = sema.type_of_pat(&pat.clone().into())?;

    if should_not_display_type_hint(sema.db, &pat, &ty) {
        return None;
    }

    acc.push(InlayHint {
        range: pat.syntax().text_range(),
        kind: InlayKind::TypeHint,
        label: ty.display_truncated(sema.db, options.max_length).to_string().into(),
    });
    Some(())
}

fn pat_is_enum_variant(db: &RootDatabase, bind_pat: &ast::BindPat, pat_ty: &Type) -> bool {
    if let Some(Adt::Enum(enum_data)) = pat_ty.as_adt() {
        let pat_text = bind_pat.syntax().to_string();
        enum_data
            .variants(db)
            .into_iter()
            .map(|variant| variant.name(db).to_string())
            .any(|enum_name| enum_name == pat_text)
    } else {
        false
    }
}

fn should_not_display_type_hint(db: &RootDatabase, bind_pat: &ast::BindPat, pat_ty: &Type) -> bool {
    if pat_ty.is_unknown() {
        return true;
    }

    if let Some(Adt::Struct(s)) = pat_ty.as_adt() {
        if s.fields(db).is_empty() && s.name(db).to_string() == bind_pat.syntax().to_string() {
            return true;
        }
    }

    for node in bind_pat.syntax().ancestors() {
        match_ast! {
            match node {
                ast::LetStmt(it) => {
                    return it.ascribed_type().is_some()
                },
                ast::Param(it) => {
                    return it.ascribed_type().is_some()
                },
                ast::MatchArm(_it) => {
                    return pat_is_enum_variant(db, bind_pat, pat_ty);
                },
                ast::IfExpr(it) => {
                    return it.condition().and_then(|condition| condition.pat()).is_some()
                        && pat_is_enum_variant(db, bind_pat, pat_ty);
                },
                ast::WhileExpr(it) => {
                    return it.condition().and_then(|condition| condition.pat()).is_some()
                        && pat_is_enum_variant(db, bind_pat, pat_ty);
                },
                _ => (),
            }
        }
    }
    false
}

fn should_show_param_hint(
    fn_signature: &FunctionSignature,
    param_name: &str,
    argument: &ast::Expr,
) -> bool {
    let argument_string = argument.syntax().to_string();
    if param_name.is_empty() || argument_string.ends_with(param_name) {
        return false;
    }

    let parameters_len = if fn_signature.has_self_param {
        fn_signature.parameters.len() - 1
    } else {
        fn_signature.parameters.len()
    };
    // avoid displaying hints for common functions like map, filter, etc.
    if parameters_len == 1 && (param_name.len() == 1 || param_name == "predicate") {
        return false;
    }

    true
}

fn get_fn_signature(sema: &Semantics<RootDatabase>, expr: &ast::Expr) -> Option<FunctionSignature> {
    match expr {
        ast::Expr::CallExpr(expr) => {
            // FIXME: Type::as_callable is broken for closures
            let callable_def = sema.type_of_expr(&expr.expr()?)?.as_callable()?;
            match callable_def {
                hir::CallableDef::FunctionId(it) => {
                    Some(FunctionSignature::from_hir(sema.db, it.into()))
                }
                hir::CallableDef::StructId(it) => {
                    FunctionSignature::from_struct(sema.db, it.into())
                }
                hir::CallableDef::EnumVariantId(it) => {
                    FunctionSignature::from_enum_variant(sema.db, it.into())
                }
            }
        }
        ast::Expr::MethodCallExpr(expr) => {
            let fn_def = sema.resolve_method_call(&expr)?;
            Some(FunctionSignature::from_hir(sema.db, fn_def))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::inlay_hints::InlayHintsOptions;
    use insta::assert_debug_snapshot;

    use crate::mock_analysis::single_file;

    #[test]
    fn param_hints_only() {
        let (analysis, file_id) = single_file(
            r#"
            fn foo(a: i32, b: i32) -> i32 { a + b }
            fn main() {
                let _x = foo(4, 4);
            }"#,
        );
        assert_debug_snapshot!(analysis.inlay_hints(file_id, &InlayHintsOptions{ parameter_hints: true, type_hints: false, chaining_hints: false, max_length: None}).unwrap(), @r###"
        [
            InlayHint {
                range: [106; 107),
                kind: ParameterHint,
                label: "a",
            },
            InlayHint {
                range: [109; 110),
                kind: ParameterHint,
                label: "b",
            },
        ]"###);
    }

    #[test]
    fn hints_disabled() {
        let (analysis, file_id) = single_file(
            r#"
            fn foo(a: i32, b: i32) -> i32 { a + b }
            fn main() {
                let _x = foo(4, 4);
            }"#,
        );
        assert_debug_snapshot!(analysis.inlay_hints(file_id, &InlayHintsOptions{ type_hints: false, parameter_hints: false, chaining_hints: false, max_length: None}).unwrap(), @r###"[]"###);
    }

    #[test]
    fn type_hints_only() {
        let (analysis, file_id) = single_file(
            r#"
            fn foo(a: i32, b: i32) -> i32 { a + b }
            fn main() {
                let _x = foo(4, 4);
            }"#,
        );
        assert_debug_snapshot!(analysis.inlay_hints(file_id, &InlayHintsOptions{ type_hints: true, parameter_hints: false, chaining_hints: false, max_length: None}).unwrap(), @r###"
        [
            InlayHint {
                range: [97; 99),
                kind: TypeHint,
                label: "i32",
            },
        ]"###);
    }
    #[test]
    fn default_generic_types_should_not_be_displayed() {
        let (analysis, file_id) = single_file(
            r#"
struct Test<K, T = u8> {
    k: K,
    t: T,
}

fn main() {
    let zz = Test { t: 23, k: 33 };
    let zz_ref = &zz;
}"#,
        );

        assert_debug_snapshot!(analysis.inlay_hints(file_id, &InlayHintsOptions::default()).unwrap(), @r###"
        [
            InlayHint {
                range: [69; 71),
                kind: TypeHint,
                label: "Test<i32>",
            },
            InlayHint {
                range: [105; 111),
                kind: TypeHint,
                label: "&Test<i32>",
            },
        ]
        "###
        );
    }

    #[test]
    fn let_statement() {
        let (analysis, file_id) = single_file(
            r#"
#[derive(PartialEq)]
enum CustomOption<T> {
    None,
    Some(T),
}

#[derive(PartialEq)]
struct Test {
    a: CustomOption<u32>,
    b: u8,
}

fn main() {
    struct InnerStruct {}

    let test = 54;
    let test: i32 = 33;
    let mut test = 33;
    let _ = 22;
    let test = "test";
    let test = InnerStruct {};

    let test = vec![222];
    let test: Vec<_> = (0..3).collect();
    let test = (0..3).collect::<Vec<i128>>();
    let test = (0..3).collect::<Vec<_>>();

    let mut test = Vec::new();
    test.push(333);

    let test = (42, 'a');
    let (a, (b, c, (d, e), f)) = (2, (3, 4, (6.6, 7.7), 5));
    let &x = &92;
}"#,
        );

        assert_debug_snapshot!(analysis.inlay_hints(file_id, &InlayHintsOptions::default()).unwrap(), @r###"
        [
            InlayHint {
                range: [193; 197),
                kind: TypeHint,
                label: "i32",
            },
            InlayHint {
                range: [236; 244),
                kind: TypeHint,
                label: "i32",
            },
            InlayHint {
                range: [275; 279),
                kind: TypeHint,
                label: "&str",
            },
            InlayHint {
                range: [539; 543),
                kind: TypeHint,
                label: "(i32, char)",
            },
            InlayHint {
                range: [566; 567),
                kind: TypeHint,
                label: "i32",
            },
            InlayHint {
                range: [570; 571),
                kind: TypeHint,
                label: "i32",
            },
            InlayHint {
                range: [573; 574),
                kind: TypeHint,
                label: "i32",
            },
            InlayHint {
                range: [577; 578),
                kind: TypeHint,
                label: "f64",
            },
            InlayHint {
                range: [580; 581),
                kind: TypeHint,
                label: "f64",
            },
            InlayHint {
                range: [584; 585),
                kind: TypeHint,
                label: "i32",
            },
            InlayHint {
                range: [627; 628),
                kind: TypeHint,
                label: "i32",
            },
        ]
        "###
        );
    }

    #[test]
    fn closure_parameters() {
        let (analysis, file_id) = single_file(
            r#"
fn main() {
    let mut start = 0;
    (0..2).for_each(|increment| {
        start += increment;
    });

    let multiply = |a, b, c, d| a * b * c * d;
    let _: i32 = multiply(1, 2, 3, 4);
    let multiply_ref = &multiply;

    let return_42 = || 42;
}"#,
        );

        assert_debug_snapshot!(analysis.inlay_hints(file_id, &InlayHintsOptions::default()).unwrap(), @r###"
        [
            InlayHint {
                range: [21; 30),
                kind: TypeHint,
                label: "i32",
            },
            InlayHint {
                range: [57; 66),
                kind: TypeHint,
                label: "i32",
            },
            InlayHint {
                range: [115; 123),
                kind: TypeHint,
                label: "|…| -> i32",
            },
            InlayHint {
                range: [127; 128),
                kind: TypeHint,
                label: "i32",
            },
            InlayHint {
                range: [130; 131),
                kind: TypeHint,
                label: "i32",
            },
            InlayHint {
                range: [133; 134),
                kind: TypeHint,
                label: "i32",
            },
            InlayHint {
                range: [136; 137),
                kind: TypeHint,
                label: "i32",
            },
            InlayHint {
                range: [201; 213),
                kind: TypeHint,
                label: "&|…| -> i32",
            },
            InlayHint {
                range: [236; 245),
                kind: TypeHint,
                label: "|| -> i32",
            },
        ]
        "###
        );
    }

    #[test]
    fn for_expression() {
        let (analysis, file_id) = single_file(
            r#"
fn main() {
    let mut start = 0;
    for increment in 0..2 {
        start += increment;
    }
}"#,
        );

        assert_debug_snapshot!(analysis.inlay_hints(file_id, &InlayHintsOptions::default()).unwrap(), @r###"
        [
            InlayHint {
                range: [21; 30),
                kind: TypeHint,
                label: "i32",
            },
            InlayHint {
                range: [44; 53),
                kind: TypeHint,
                label: "i32",
            },
        ]
        "###
        );
    }

    #[test]
    fn if_expr() {
        let (analysis, file_id) = single_file(
            r#"
#[derive(PartialEq)]
enum CustomOption<T> {
    None,
    Some(T),
}

#[derive(PartialEq)]
struct Test {
    a: CustomOption<u32>,
    b: u8,
}

use CustomOption::*;

fn main() {
    let test = Some(Test { a: Some(3), b: 1 });
    if let None = &test {};
    if let test = &test {};
    if let Some(test) = &test {};
    if let Some(Test { a, b }) = &test {};
    if let Some(Test { a: x, b: y }) = &test {};
    if let Some(Test { a: Some(x), b: y }) = &test {};
    if let Some(Test { a: None, b: y }) = &test {};
    if let Some(Test { b: y, .. }) = &test {};

    if test == None {}
}"#,
        );

        assert_debug_snapshot!(analysis.inlay_hints(file_id, &InlayHintsOptions::default()).unwrap(), @r###"
        [
            InlayHint {
                range: [188; 192),
                kind: TypeHint,
                label: "CustomOption<Test>",
            },
            InlayHint {
                range: [267; 271),
                kind: TypeHint,
                label: "&CustomOption<Test>",
            },
            InlayHint {
                range: [300; 304),
                kind: TypeHint,
                label: "&Test",
            },
            InlayHint {
                range: [341; 342),
                kind: TypeHint,
                label: "&CustomOption<u32>",
            },
            InlayHint {
                range: [344; 345),
                kind: TypeHint,
                label: "&u8",
            },
            InlayHint {
                range: [387; 388),
                kind: TypeHint,
                label: "&CustomOption<u32>",
            },
            InlayHint {
                range: [393; 394),
                kind: TypeHint,
                label: "&u8",
            },
            InlayHint {
                range: [441; 442),
                kind: TypeHint,
                label: "&u32",
            },
            InlayHint {
                range: [448; 449),
                kind: TypeHint,
                label: "&u8",
            },
            InlayHint {
                range: [500; 501),
                kind: TypeHint,
                label: "&u8",
            },
            InlayHint {
                range: [543; 544),
                kind: TypeHint,
                label: "&u8",
            },
        ]
        "###
        );
    }

    #[test]
    fn while_expr() {
        let (analysis, file_id) = single_file(
            r#"
#[derive(PartialEq)]
enum CustomOption<T> {
    None,
    Some(T),
}

#[derive(PartialEq)]
struct Test {
    a: CustomOption<u32>,
    b: u8,
}

use CustomOption::*;

fn main() {
    let test = Some(Test { a: Some(3), b: 1 });
    while let None = &test {};
    while let test = &test {};
    while let Some(test) = &test {};
    while let Some(Test { a, b }) = &test {};
    while let Some(Test { a: x, b: y }) = &test {};
    while let Some(Test { a: Some(x), b: y }) = &test {};
    while let Some(Test { a: None, b: y }) = &test {};
    while let Some(Test { b: y, .. }) = &test {};

    while test == None {}
}"#,
        );

        assert_debug_snapshot!(analysis.inlay_hints(file_id, &InlayHintsOptions::default()).unwrap(), @r###"
        [
            InlayHint {
                range: [188; 192),
                kind: TypeHint,
                label: "CustomOption<Test>",
            },
            InlayHint {
                range: [273; 277),
                kind: TypeHint,
                label: "&CustomOption<Test>",
            },
            InlayHint {
                range: [309; 313),
                kind: TypeHint,
                label: "&Test",
            },
            InlayHint {
                range: [353; 354),
                kind: TypeHint,
                label: "&CustomOption<u32>",
            },
            InlayHint {
                range: [356; 357),
                kind: TypeHint,
                label: "&u8",
            },
            InlayHint {
                range: [402; 403),
                kind: TypeHint,
                label: "&CustomOption<u32>",
            },
            InlayHint {
                range: [408; 409),
                kind: TypeHint,
                label: "&u8",
            },
            InlayHint {
                range: [459; 460),
                kind: TypeHint,
                label: "&u32",
            },
            InlayHint {
                range: [466; 467),
                kind: TypeHint,
                label: "&u8",
            },
            InlayHint {
                range: [521; 522),
                kind: TypeHint,
                label: "&u8",
            },
            InlayHint {
                range: [567; 568),
                kind: TypeHint,
                label: "&u8",
            },
        ]
        "###
        );
    }

    #[test]
    fn match_arm_list() {
        let (analysis, file_id) = single_file(
            r#"
#[derive(PartialEq)]
enum CustomOption<T> {
    None,
    Some(T),
}

#[derive(PartialEq)]
struct Test {
    a: CustomOption<u32>,
    b: u8,
}

use CustomOption::*;

fn main() {
    match Some(Test { a: Some(3), b: 1 }) {
        None => (),
        test => (),
        Some(test) => (),
        Some(Test { a, b }) => (),
        Some(Test { a: x, b: y }) => (),
        Some(Test { a: Some(x), b: y }) => (),
        Some(Test { a: None, b: y }) => (),
        Some(Test { b: y, .. }) => (),
        _ => {}
    }
}"#,
        );

        assert_debug_snapshot!(analysis.inlay_hints(file_id, &InlayHintsOptions::default()).unwrap(), @r###"
        [
            InlayHint {
                range: [252; 256),
                kind: TypeHint,
                label: "CustomOption<Test>",
            },
            InlayHint {
                range: [277; 281),
                kind: TypeHint,
                label: "Test",
            },
            InlayHint {
                range: [310; 311),
                kind: TypeHint,
                label: "CustomOption<u32>",
            },
            InlayHint {
                range: [313; 314),
                kind: TypeHint,
                label: "u8",
            },
            InlayHint {
                range: [348; 349),
                kind: TypeHint,
                label: "CustomOption<u32>",
            },
            InlayHint {
                range: [354; 355),
                kind: TypeHint,
                label: "u8",
            },
            InlayHint {
                range: [394; 395),
                kind: TypeHint,
                label: "u32",
            },
            InlayHint {
                range: [401; 402),
                kind: TypeHint,
                label: "u8",
            },
            InlayHint {
                range: [445; 446),
                kind: TypeHint,
                label: "u8",
            },
            InlayHint {
                range: [480; 481),
                kind: TypeHint,
                label: "u8",
            },
        ]
        "###
        );
    }

    #[test]
    fn hint_truncation() {
        let (analysis, file_id) = single_file(
            r#"
struct Smol<T>(T);

struct VeryLongOuterName<T>(T);

fn main() {
    let a = Smol(0u32);
    let b = VeryLongOuterName(0usize);
    let c = Smol(Smol(0u32))
}"#,
        );

        assert_debug_snapshot!(analysis.inlay_hints(file_id, &InlayHintsOptions { max_length: Some(8), ..Default::default() }).unwrap(), @r###"
        [
            InlayHint {
                range: [74; 75),
                kind: TypeHint,
                label: "Smol<u32>",
            },
            InlayHint {
                range: [98; 99),
                kind: TypeHint,
                label: "VeryLongOuterName<…>",
            },
            InlayHint {
                range: [137; 138),
                kind: TypeHint,
                label: "Smol<Smol<…>>",
            },
        ]
        "###
        );
    }

    #[test]
    fn function_call_parameter_hint() {
        let (analysis, file_id) = single_file(
            r#"
enum CustomOption<T> {
    None,
    Some(T),
}
use CustomOption::*;

struct FileId {}
struct SmolStr {}

impl From<&str> for SmolStr {
    fn from(_: &str) -> Self {
        unimplemented!()
    }
}

struct TextRange {}
struct SyntaxKind {}
struct NavigationTarget {}

struct Test {}

impl Test {
    fn method(&self, mut param: i32) -> i32 {
        param * 2
    }

    fn from_syntax(
        file_id: FileId,
        name: SmolStr,
        focus_range: CustomOption<TextRange>,
        full_range: TextRange,
        kind: SyntaxKind,
        docs: CustomOption<String>,
        description: CustomOption<String>,
    ) -> NavigationTarget {
        NavigationTarget {}
    }
}

fn test_func(mut foo: i32, bar: i32, msg: &str, _: i32, last: i32) -> i32 {
    foo + bar
}

fn main() {
    let not_literal = 1;
    let _: i32 = test_func(1, 2, "hello", 3, not_literal);
    let t: Test = Test {};
    t.method(123);
    Test::method(&t, 3456);

    Test::from_syntax(
        FileId {},
        "impl".into(),
        None,
        TextRange {},
        SyntaxKind {},
        None,
        None,
    );
}"#,
        );

        assert_debug_snapshot!(analysis.inlay_hints(file_id, &InlayHintsOptions::default()).unwrap(), @r###"
        [
            InlayHint {
                range: [798; 809),
                kind: TypeHint,
                label: "i32",
            },
            InlayHint {
                range: [842; 843),
                kind: ParameterHint,
                label: "foo",
            },
            InlayHint {
                range: [845; 846),
                kind: ParameterHint,
                label: "bar",
            },
            InlayHint {
                range: [848; 855),
                kind: ParameterHint,
                label: "msg",
            },
            InlayHint {
                range: [860; 871),
                kind: ParameterHint,
                label: "last",
            },
            InlayHint {
                range: [914; 917),
                kind: ParameterHint,
                label: "param",
            },
            InlayHint {
                range: [937; 939),
                kind: ParameterHint,
                label: "&self",
            },
            InlayHint {
                range: [941; 945),
                kind: ParameterHint,
                label: "param",
            },
            InlayHint {
                range: [980; 989),
                kind: ParameterHint,
                label: "file_id",
            },
            InlayHint {
                range: [999; 1012),
                kind: ParameterHint,
                label: "name",
            },
            InlayHint {
                range: [1022; 1026),
                kind: ParameterHint,
                label: "focus_range",
            },
            InlayHint {
                range: [1036; 1048),
                kind: ParameterHint,
                label: "full_range",
            },
            InlayHint {
                range: [1058; 1071),
                kind: ParameterHint,
                label: "kind",
            },
            InlayHint {
                range: [1081; 1085),
                kind: ParameterHint,
                label: "docs",
            },
            InlayHint {
                range: [1095; 1099),
                kind: ParameterHint,
                label: "description",
            },
        ]
        "###
        );
    }

    #[test]
    fn omitted_parameters_hints_heuristics() {
        let (analysis, file_id) = single_file(
            r#"
fn map(f: i32) {}
fn filter(predicate: i32) {}

struct TestVarContainer {
    test_var: i32,
}

struct Test {}

impl Test {
    fn map(self, f: i32) -> Self {
        self
    }

    fn filter(self, predicate: i32) -> Self {
        self
    }

    fn no_hints_expected(&self, _: i32, test_var: i32) {}
}

fn main() {
    let container: TestVarContainer = TestVarContainer { test_var: 42 };
    let test: Test = Test {};

    map(22);
    filter(33);

    let test_processed: Test = test.map(1).filter(2);

    let test_var: i32 = 55;
    test_processed.no_hints_expected(22, test_var);
    test_processed.no_hints_expected(33, container.test_var);
}"#,
        );

        assert_debug_snapshot!(analysis.inlay_hints(file_id, &InlayHintsOptions { max_length: Some(8), ..Default::default() }).unwrap(), @r###"
        []
        "###
        );
    }

    #[test]
    fn unit_structs_have_no_type_hints() {
        let (analysis, file_id) = single_file(
            r#"
enum CustomResult<T, E> {
    Ok(T),
    Err(E),
}
use CustomResult::*;

struct SyntheticSyntax;

fn main() {
    match Ok(()) {
        Ok(_) => (),
        Err(SyntheticSyntax) => (),
    }
}"#,
        );

        assert_debug_snapshot!(analysis.inlay_hints(file_id, &InlayHintsOptions { max_length: Some(8), ..Default::default() }).unwrap(), @r###"
        []
        "###
        );
    }

    #[test]
    fn chaining_hints_ignore_comments() {
        let (analysis, file_id) = single_file(
            r#"
            struct A(B);
            impl A { fn into_b(self) -> B { self.0 } }
            struct B(C);
            impl B { fn into_c(self) -> C { self.0 } }
            struct C;

            fn main() {
                let c = A(B(C))
                    .into_b() // This is a comment
                    .into_c();
            }"#,
        );
        assert_debug_snapshot!(analysis.inlay_hints(file_id, &InlayHintsOptions{ parameter_hints: false, type_hints: false, chaining_hints: true, max_length: None}).unwrap(), @r###"
        [
            InlayHint {
                range: [232; 269),
                kind: ChainingHint,
                label: "B",
            },
            InlayHint {
                range: [232; 239),
                kind: ChainingHint,
                label: "A",
            },
        ]"###);
    }

    #[test]
    fn chaining_hints_without_newlines() {
        let (analysis, file_id) = single_file(
            r#"
            struct A(B);
            impl A { fn into_b(self) -> B { self.0 } }
            struct B(C);
            impl B { fn into_c(self) -> C { self.0 } }
            struct C;

            fn main() {
                let c = A(B(C)).into_b().into_c();
            }"#,
        );
        assert_debug_snapshot!(analysis.inlay_hints(file_id, &InlayHintsOptions{ parameter_hints: false, type_hints: false, chaining_hints: true, max_length: None}).unwrap(), @r###"[]"###);
    }

    #[test]
    fn struct_access_chaining_hints() {
        let (analysis, file_id) = single_file(
            r#"
            struct A { pub b: B }
            struct B { pub c: C }
            struct C(pub bool);

            fn main() {
                let x = A { b: B { c: C(true) } }
                    .b
                    .c
                    .0;
            }"#,
        );
        assert_debug_snapshot!(analysis.inlay_hints(file_id, &InlayHintsOptions{ parameter_hints: false, type_hints: false, chaining_hints: true, max_length: None}).unwrap(), @r###"
        [
            InlayHint {
                range: [150; 221),
                kind: ChainingHint,
                label: "C",
            },
            InlayHint {
                range: [150; 198),
                kind: ChainingHint,
                label: "B",
            },
            InlayHint {
                range: [150; 175),
                kind: ChainingHint,
                label: "A",
            },
        ]"###);
    }

    #[test]
    fn generic_chaining_hints() {
        let (analysis, file_id) = single_file(
            r#"
            struct A<T>(T);
            struct B<T>(T);
            struct C<T>(T);
            struct X<T,R>(T, R);

            impl<T> A<T> {
                fn new(t: T) -> Self { A(t) }
                fn into_b(self) -> B<T> { B(self.0) }
            }
            impl<T> B<T> {
                fn into_c(self) -> C<T> { C(self.0) }
            }
            fn main() {
                let c = A::new(X(42, true))
                    .into_b()
                    .into_c();
            }"#,
        );
        assert_debug_snapshot!(analysis.inlay_hints(file_id, &InlayHintsOptions{ parameter_hints: false, type_hints: false, chaining_hints: true, max_length: None}).unwrap(), @r###"
        [
            InlayHint {
                range: [403; 452),
                kind: ChainingHint,
                label: "B<X<i32, bool>>",
            },
            InlayHint {
                range: [403; 422),
                kind: ChainingHint,
                label: "A<X<i32, bool>>",
            },
        ]"###);
    }
}
