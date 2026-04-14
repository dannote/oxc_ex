use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use oxc95::minifier::{
    CompressOptions as RolldownCompressOptions, MangleOptions as RolldownMangleOptions,
    MangleOptionsKeepNames as RolldownMangleOptionsKeepNames,
};
use oxc_allocator::Allocator;
use oxc_ast::ast::{Expression, ImportOrExportKind, Statement};
use oxc_codegen::{Codegen, CodegenOptions, CodegenReturn};
use oxc_diagnostics::OxcDiagnostic;
use oxc_minifier::{CompressOptions, MangleOptions, Minifier, MinifierOptions};
use oxc_parser::{ParseOptions, Parser};
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;
use oxc_transformer::{EnvOptions, JsxRuntime, TransformOptions, Transformer};
use rolldown::{
    AddonOutputOption, Bundler, BundlerOptions, BundlerTransformOptions, Either as RolldownEither,
    InputItem, IsExternal, JsxOptions as RolldownJsxOptions, OutputFormat, RawMinifyOptions,
    RawMinifyOptionsDetailed, ResolveOptions as RolldownResolveOptions, SourceMapType,
    TreeshakeOptions,
};
use rolldown_common::Output;
use rustler::{serde::from_term, Encoder, Env, NifResult, SerdeTerm, Term};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use tempfile::TempDir;
use tokio::runtime::Builder as RuntimeBuilder;

mod atoms {
    rustler::atoms! {
        ok,
        error,
        specifier,
        atom_type = "type",
        kind,
        start,
        atom_end = "end",
        atom_static = "static",
        dynamic,
        import,
        export,
        export_all,
    }
}

fn parser_options() -> ParseOptions {
    ParseOptions {
        parse_regular_expression: true,
        ..ParseOptions::default()
    }
}

fn format_errors(errors: &[OxcDiagnostic]) -> Vec<String> {
    errors.iter().map(ToString::to_string).collect()
}

#[derive(Serialize)]
struct MessageError {
    message: String,
}

#[derive(Serialize)]
struct CodeWithSourcemap {
    code: String,
    sourcemap: String,
}

fn encode_ok<'a, T: Serialize>(env: Env<'a>, value: T) -> NifResult<Term<'a>> {
    Ok((atoms::ok(), SerdeTerm(value)).encode(env))
}

fn error_messages_to_term<'a>(env: Env<'a>, messages: &[String]) -> NifResult<Term<'a>> {
    Ok((atoms::error(), messages).encode(env))
}

fn parse_errors_to_term<'a>(env: Env<'a>, messages: Vec<String>) -> NifResult<Term<'a>> {
    let errors = messages
        .into_iter()
        .map(|message| MessageError { message })
        .collect::<Vec<_>>();
    Ok((atoms::error(), SerdeTerm(errors)).encode(env))
}

fn decode_options<T: DeserializeOwned + Default>(term: Term<'_>) -> T {
    from_term::<Value>(term)
        .ok()
        .and_then(|value| serde_json::from_value::<T>(value).ok())
        .unwrap_or_default()
}

fn build_transform_options(
    jsx_runtime: &str,
    jsx_factory: &str,
    jsx_fragment: &str,
    import_source: &str,
    target: &str,
) -> TransformOptions {
    let mut options = TransformOptions::default();
    options.jsx.runtime = match jsx_runtime {
        "classic" => JsxRuntime::Classic,
        _ => JsxRuntime::Automatic,
    };
    if !jsx_factory.is_empty() {
        options.jsx.pragma = Some(jsx_factory.to_string());
    }
    if !jsx_fragment.is_empty() {
        options.jsx.pragma_frag = Some(jsx_fragment.to_string());
    }
    if !import_source.is_empty() {
        options.jsx.import_source = Some(import_source.to_string());
    }
    if !target.is_empty() {
        if let Ok(env) = EnvOptions::from_target(target) {
            options.env = env;
        }
    }
    options
}

#[rustler::nif(schedule = "DirtyCpu")]
fn parse<'a>(env: Env<'a>, source: &str, filename: &str) -> NifResult<Term<'a>> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(filename).unwrap_or_default();
    let ret = Parser::new(&allocator, source, source_type)
        .with_options(parser_options())
        .parse();

    if !ret.errors.is_empty() {
        return parse_errors_to_term(env, format_errors(&ret.errors));
    }

    let json = serde_json::from_str::<Value>(&ret.program.to_estree_ts_json(false)).unwrap();
    encode_ok(env, json)
}

#[rustler::nif(schedule = "DirtyCpu")]
fn valid(source: &str, filename: &str) -> bool {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(filename).unwrap_or_default();
    let ret = Parser::new(&allocator, source, source_type).parse();
    ret.errors.is_empty()
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(default)]
struct TransformInput {
    #[serde(rename = "jsx", default = "default_jsx_runtime")]
    jsx_runtime: String,
    jsx_factory: String,
    jsx_fragment: String,
    import_source: String,
    target: String,
    sourcemap: bool,
}

impl Default for TransformInput {
    fn default() -> Self {
        Self {
            jsx_runtime: default_jsx_runtime(),
            jsx_factory: String::new(),
            jsx_fragment: String::new(),
            import_source: String::new(),
            target: String::new(),
            sourcemap: false,
        }
    }
}

#[derive(Deserialize)]
#[serde(default)]
struct MinifyInput {
    #[serde(default = "default_true")]
    mangle: bool,
}

impl Default for MinifyInput {
    fn default() -> Self {
        Self { mangle: true }
    }
}

#[rustler::nif(schedule = "DirtyCpu")]
fn transform<'a>(
    env: Env<'a>,
    source: &str,
    filename: &str,
    opts_term: Term<'a>,
) -> NifResult<Term<'a>> {
    let opts = decode_options::<TransformInput>(opts_term);
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(filename).unwrap_or_default();
    let path = Path::new(filename);

    let ret = Parser::new(&allocator, source, source_type)
        .with_options(parser_options())
        .parse();

    if !ret.errors.is_empty() {
        return error_messages_to_term(env, &format_errors(&ret.errors));
    }

    let mut program = ret.program;
    let scoping = SemanticBuilder::new()
        .build(&program)
        .semantic
        .into_scoping();

    let options = build_transform_options(
        &opts.jsx_runtime,
        &opts.jsx_factory,
        &opts.jsx_fragment,
        &opts.import_source,
        &opts.target,
    );
    let result =
        Transformer::new(&allocator, path, &options).build_with_scoping(scoping, &mut program);

    if !result.errors.is_empty() {
        return error_messages_to_term(env, &format_errors(&result.errors));
    }

    if opts.sourcemap {
        let CodegenReturn { code, map, .. } = Codegen::new()
            .with_options(CodegenOptions {
                source_map_path: Some(PathBuf::from(filename)),
                ..CodegenOptions::default()
            })
            .build(&program);

        if let Some(map) = map {
            encode_ok(
                env,
                CodeWithSourcemap {
                    code,
                    sourcemap: map.to_json_string(),
                },
            )
        } else {
            encode_ok(env, code)
        }
    } else {
        let CodegenReturn { code, .. } = Codegen::new().build(&program);
        encode_ok(env, code)
    }
}

#[rustler::nif(schedule = "DirtyCpu")]
fn minify<'a>(
    env: Env<'a>,
    source: &str,
    filename: &str,
    opts_term: Term<'a>,
) -> NifResult<Term<'a>> {
    let opts = decode_options::<MinifyInput>(opts_term);
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(filename).unwrap_or_default();

    let ret = Parser::new(&allocator, source, source_type)
        .with_options(parser_options())
        .parse();

    if !ret.errors.is_empty() {
        return error_messages_to_term(env, &format_errors(&ret.errors));
    }

    let mut program = ret.program;
    let result = Minifier::new(MinifierOptions {
        mangle: opts.mangle.then(MangleOptions::default),
        compress: Some(CompressOptions::default()),
    })
    .minify(&allocator, &mut program);

    let CodegenReturn { code, .. } = Codegen::new()
        .with_options(CodegenOptions::minify())
        .with_scoping(result.scoping)
        .build(&program);

    encode_ok(env, code)
}

#[rustler::nif(schedule = "DirtyCpu")]
fn imports<'a>(env: Env<'a>, source: &str, filename: &str) -> NifResult<Term<'a>> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(filename).unwrap_or_default();
    let ret = Parser::new(&allocator, source, source_type)
        .with_options(parser_options())
        .parse();

    if !ret.errors.is_empty() {
        return error_messages_to_term(env, &format_errors(&ret.errors));
    }

    let specifiers = ret
        .program
        .body
        .iter()
        .filter_map(|stmt| match stmt {
            Statement::ImportDeclaration(decl) if decl.import_kind != ImportOrExportKind::Type => {
                Some(decl.source.value.to_string())
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    encode_ok(env, specifiers)
}

struct ImportInfo {
    specifier: String,
    import_type: ImportType,
    kind: ImportKind,
    start: u32,
    end: u32,
}

enum ImportType {
    Static,
    Dynamic,
}

enum ImportKind {
    Import,
    Export,
    ExportAll,
}

impl Encoder for ImportInfo {
    fn encode<'a>(&self, env: Env<'a>) -> Term<'a> {
        rustler::Term::map_new(env)
            .map_put(atoms::specifier().encode(env), self.specifier.encode(env))
            .unwrap()
            .map_put(
                atoms::atom_type().encode(env),
                match self.import_type {
                    ImportType::Static => atoms::atom_static().encode(env),
                    ImportType::Dynamic => atoms::dynamic().encode(env),
                },
            )
            .unwrap()
            .map_put(
                atoms::kind().encode(env),
                match self.kind {
                    ImportKind::Import => atoms::import().encode(env),
                    ImportKind::Export => atoms::export().encode(env),
                    ImportKind::ExportAll => atoms::export_all().encode(env),
                },
            )
            .unwrap()
            .map_put(atoms::start().encode(env), self.start.encode(env))
            .unwrap()
            .map_put(atoms::atom_end().encode(env), self.end.encode(env))
            .unwrap()
    }
}

fn collect_imports_from_expression(expr: &Expression, results: &mut Vec<ImportInfo>) {
    match expr {
        Expression::ImportExpression(import_expr) => {
            if let Expression::StringLiteral(lit) = &import_expr.source {
                results.push(ImportInfo {
                    specifier: lit.value.to_string(),
                    import_type: ImportType::Dynamic,
                    kind: ImportKind::Import,
                    start: lit.span.start,
                    end: lit.span.end,
                });
            }
        }
        Expression::SequenceExpression(seq) => {
            for e in &seq.expressions {
                collect_imports_from_expression(e, results);
            }
        }
        Expression::ConditionalExpression(cond) => {
            collect_imports_from_expression(&cond.test, results);
            collect_imports_from_expression(&cond.consequent, results);
            collect_imports_from_expression(&cond.alternate, results);
        }
        Expression::CallExpression(call) => {
            collect_imports_from_expression(&call.callee, results);
            for arg in &call.arguments {
                if let Some(e) = arg.as_expression() {
                    collect_imports_from_expression(e, results);
                }
            }
        }
        Expression::NewExpression(new_expr) => {
            collect_imports_from_expression(&new_expr.callee, results);
            for arg in &new_expr.arguments {
                if let Some(e) = arg.as_expression() {
                    collect_imports_from_expression(e, results);
                }
            }
        }
        Expression::AssignmentExpression(assign) => {
            collect_imports_from_expression(&assign.right, results);
        }
        Expression::AwaitExpression(await_expr) => {
            collect_imports_from_expression(&await_expr.argument, results);
        }
        Expression::LogicalExpression(logical) => {
            collect_imports_from_expression(&logical.left, results);
            collect_imports_from_expression(&logical.right, results);
        }
        Expression::BinaryExpression(binary) => {
            collect_imports_from_expression(&binary.left, results);
            collect_imports_from_expression(&binary.right, results);
        }
        Expression::UnaryExpression(unary) => {
            collect_imports_from_expression(&unary.argument, results);
        }
        Expression::ParenthesizedExpression(paren) => {
            collect_imports_from_expression(&paren.expression, results);
        }
        Expression::ArrowFunctionExpression(arrow) => {
            collect_imports_from_statements(&arrow.body.statements, results);
        }
        Expression::TemplateLiteral(_) => {}
        Expression::TaggedTemplateExpression(tagged) => {
            collect_imports_from_expression(&tagged.tag, results);
        }
        Expression::ComputedMemberExpression(member) => {
            collect_imports_from_expression(&member.object, results);
            collect_imports_from_expression(&member.expression, results);
        }
        Expression::StaticMemberExpression(member) => {
            collect_imports_from_expression(&member.object, results);
        }
        Expression::ObjectExpression(obj) => {
            for prop in &obj.properties {
                match prop {
                    oxc_ast::ast::ObjectPropertyKind::ObjectProperty(p) => {
                        collect_imports_from_expression(&p.value, results);
                    }
                    oxc_ast::ast::ObjectPropertyKind::SpreadProperty(spread) => {
                        collect_imports_from_expression(&spread.argument, results);
                    }
                }
            }
        }
        Expression::ArrayExpression(arr) => {
            for elem in &arr.elements {
                match elem {
                    oxc_ast::ast::ArrayExpressionElement::SpreadElement(spread) => {
                        collect_imports_from_expression(&spread.argument, results);
                    }
                    _ => {
                        if let Some(e) = elem.as_expression() {
                            collect_imports_from_expression(e, results);
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn collect_imports_from_statement(stmt: &Statement, results: &mut Vec<ImportInfo>) {
    collect_imports_from_statements(std::slice::from_ref(stmt), results);
}

fn collect_imports_from_statements(stmts: &[Statement], results: &mut Vec<ImportInfo>) {
    for stmt in stmts {
        match stmt {
            Statement::ImportDeclaration(decl) => {
                if decl.import_kind != ImportOrExportKind::Type {
                    results.push(ImportInfo {
                        specifier: decl.source.value.to_string(),
                        import_type: ImportType::Static,
                        kind: ImportKind::Import,
                        start: decl.source.span.start,
                        end: decl.source.span.end,
                    });
                }
            }
            Statement::ExportNamedDeclaration(decl) => {
                if decl.export_kind != ImportOrExportKind::Type {
                    if let Some(source) = &decl.source {
                        results.push(ImportInfo {
                            specifier: source.value.to_string(),
                            import_type: ImportType::Static,
                            kind: ImportKind::Export,
                            start: source.span.start,
                            end: source.span.end,
                        });
                    }
                }
            }
            Statement::ExportAllDeclaration(decl) => {
                if decl.export_kind != ImportOrExportKind::Type {
                    results.push(ImportInfo {
                        specifier: decl.source.value.to_string(),
                        import_type: ImportType::Static,
                        kind: ImportKind::ExportAll,
                        start: decl.source.span.start,
                        end: decl.source.span.end,
                    });
                }
            }
            Statement::ExpressionStatement(expr_stmt) => {
                collect_imports_from_expression(&expr_stmt.expression, results);
            }
            Statement::VariableDeclaration(var_decl) => {
                for declarator in &var_decl.declarations {
                    if let Some(init) = &declarator.init {
                        collect_imports_from_expression(init, results);
                    }
                }
            }
            Statement::ReturnStatement(ret) => {
                if let Some(arg) = &ret.argument {
                    collect_imports_from_expression(arg, results);
                }
            }
            Statement::IfStatement(if_stmt) => {
                collect_imports_from_statement(&if_stmt.consequent, results);
                if let Some(alt) = &if_stmt.alternate {
                    collect_imports_from_statement(alt, results);
                }
            }
            Statement::BlockStatement(block) => {
                collect_imports_from_statements(&block.body, results);
            }
            Statement::FunctionDeclaration(func) => {
                if let Some(body) = &func.body {
                    collect_imports_from_statements(&body.statements, results);
                }
            }
            Statement::SwitchStatement(switch) => {
                for case in &switch.cases {
                    collect_imports_from_statements(&case.consequent, results);
                }
            }
            Statement::TryStatement(try_stmt) => {
                collect_imports_from_statements(&try_stmt.block.body, results);
                if let Some(handler) = &try_stmt.handler {
                    collect_imports_from_statements(&handler.body.body, results);
                }
                if let Some(finalizer) = &try_stmt.finalizer {
                    collect_imports_from_statements(&finalizer.body, results);
                }
            }
            Statement::ForStatement(for_stmt) => {
                collect_imports_from_statement(&for_stmt.body, results);
            }
            Statement::ForInStatement(for_in) => {
                collect_imports_from_statement(&for_in.body, results);
            }
            Statement::ForOfStatement(for_of) => {
                collect_imports_from_statement(&for_of.body, results);
            }
            Statement::WhileStatement(while_stmt) => {
                collect_imports_from_statement(&while_stmt.body, results);
            }
            Statement::ExportDefaultDeclaration(decl) => {
                if let oxc_ast::ast::ExportDefaultDeclarationKind::FunctionDeclaration(func) =
                    &decl.declaration
                {
                    if let Some(body) = &func.body {
                        collect_imports_from_statements(&body.statements, results);
                    }
                }
            }
            _ => {}
        }
    }
}

#[rustler::nif(schedule = "DirtyCpu")]
fn collect_imports<'a>(env: Env<'a>, source: &str, filename: &str) -> NifResult<Term<'a>> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(filename).unwrap_or_default();
    let ret = Parser::new(&allocator, source, source_type)
        .with_options(parser_options())
        .parse();

    if !ret.errors.is_empty() {
        return error_messages_to_term(env, &format_errors(&ret.errors));
    }

    let mut results: Vec<ImportInfo> = Vec::new();
    collect_imports_from_statements(&ret.program.body, &mut results);

    Ok((atoms::ok(), results).encode(env))
}

fn default_jsx_runtime() -> String {
    "automatic".to_string()
}

fn default_format() -> String {
    "iife".to_string()
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct BundleOptions {
    entry: String,
    #[serde(default = "default_format")]
    format: String,
    minify: bool,
    banner: Option<String>,
    footer: Option<String>,
    preamble: Option<String>,
    define: BTreeMap<String, String>,
    sourcemap: bool,
    drop_console: bool,
    #[serde(rename = "jsx", default = "default_jsx_runtime")]
    jsx_runtime: String,
    jsx_factory: String,
    jsx_fragment: String,
    import_source: String,
    target: String,
}

fn normalize_virtual_path(path: &str) -> Result<PathBuf, String> {
    let normalized = path.replace('\\', "/");
    let mut result = PathBuf::new();

    for component in Path::new(&normalized).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            Component::Normal(part) => result.push(part),
            Component::RootDir | Component::Prefix(_) => {}
        }
    }

    if result.as_os_str().is_empty() {
        return Err(format!("Invalid virtual filename: {path:?}"));
    }

    Ok(result)
}

fn is_bare_specifier(specifier: &str) -> bool {
    !specifier.starts_with('.') && !specifier.starts_with('/')
}

fn collect_external_specifiers(files: &[(String, String)]) -> Result<Vec<String>, Vec<String>> {
    let mut specifiers = BTreeSet::new();

    for (filename, source) in files {
        let allocator = Allocator::default();
        let source_type = SourceType::from_path(filename).unwrap_or_default();
        let ret = Parser::new(&allocator, source, source_type)
            .with_options(parser_options())
            .parse();

        if !ret.errors.is_empty() {
            return Err(format_errors(&ret.errors));
        }

        for statement in ret.program.body.iter() {
            match statement {
                Statement::ImportDeclaration(decl)
                    if decl.import_kind != ImportOrExportKind::Type
                        && is_bare_specifier(decl.source.value.as_str()) =>
                {
                    specifiers.insert(decl.source.value.to_string());
                }
                Statement::ExportAllDeclaration(decl)
                    if decl.export_kind != ImportOrExportKind::Type
                        && is_bare_specifier(decl.source.value.as_str()) =>
                {
                    specifiers.insert(decl.source.value.to_string());
                }
                Statement::ExportNamedDeclaration(decl) => {
                    if decl.export_kind != ImportOrExportKind::Type {
                        if let Some(source) = &decl.source {
                            if is_bare_specifier(source.value.as_str()) {
                                specifiers.insert(source.value.to_string());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    Ok(specifiers.into_iter().collect())
}

fn write_virtual_project(
    tempdir: &TempDir,
    files: &[(String, String)],
) -> Result<Vec<String>, Vec<String>> {
    let mut written = BTreeSet::new();

    for (filename, source) in files {
        let relative_path = match normalize_virtual_path(filename) {
            Ok(path) => path,
            Err(message) => return Err(vec![message]),
        };
        let import_path = relative_path.to_string_lossy().replace('\\', "/");

        if !written.insert(import_path.clone()) {
            return Err(vec![format!(
                "Duplicate module path after normalization: {filename:?}"
            )]);
        }

        let full_path = tempdir.path().join(&relative_path);
        if let Some(parent) = full_path.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                return Err(vec![format!(
                    "Failed to create directory for {filename:?}: {error}"
                )]);
            }
        }

        if let Err(error) = fs::write(&full_path, source) {
            return Err(vec![format!("Failed to write {filename:?}: {error}")]);
        }
    }

    Ok(written.into_iter().collect())
}

fn build_rolldown_resolve_options() -> RolldownResolveOptions {
    RolldownResolveOptions {
        extensions: Some(vec![
            ".tsx".to_string(),
            ".ts".to_string(),
            ".jsx".to_string(),
            ".js".to_string(),
            ".json".to_string(),
        ]),
        extension_alias: Some(vec![
            (
                ".js".to_string(),
                vec![
                    ".ts".to_string(),
                    ".tsx".to_string(),
                    ".js".to_string(),
                    ".jsx".to_string(),
                ],
            ),
            (
                ".jsx".to_string(),
                vec![
                    ".tsx".to_string(),
                    ".ts".to_string(),
                    ".jsx".to_string(),
                    ".js".to_string(),
                ],
            ),
        ]),
        ..RolldownResolveOptions::default()
    }
}

fn build_rolldown_transform_options(opts: &BundleOptions) -> BundlerTransformOptions {
    let jsx = (opts.jsx_runtime != "automatic"
        || !opts.jsx_factory.is_empty()
        || !opts.jsx_fragment.is_empty()
        || !opts.import_source.is_empty())
    .then(|| {
        RolldownEither::Right(RolldownJsxOptions {
            runtime: Some(opts.jsx_runtime.clone()),
            import_source: (!opts.import_source.is_empty()).then(|| opts.import_source.clone()),
            pragma: (!opts.jsx_factory.is_empty()).then(|| opts.jsx_factory.clone()),
            pragma_frag: (!opts.jsx_fragment.is_empty()).then(|| opts.jsx_fragment.clone()),
            ..RolldownJsxOptions::default()
        })
    });

    BundlerTransformOptions {
        jsx,
        target: (!opts.target.is_empty()).then(|| RolldownEither::Left(opts.target.clone())),
        ..BundlerTransformOptions::default()
    }
}

fn build_minify_options(drop_console: bool) -> RawMinifyOptions {
    if !drop_console {
        return RawMinifyOptions::Bool(true);
    }

    let compress = RolldownCompressOptions {
        drop_console: true,
        ..RolldownCompressOptions::smallest()
    };
    let mangle = RolldownMangleOptions {
        top_level: false,
        keep_names: RolldownMangleOptionsKeepNames::all_false(),
        debug: false,
    };

    RawMinifyOptions::Object(RawMinifyOptionsDetailed {
        options: oxc95::minifier::MinifierOptions {
            mangle: Some(mangle),
            compress: Some(compress),
        },
        default_target: true,
        remove_whitespace: true,
    })
}

fn relativize_sourcemap_sources(sourcemap_json: String, cwd: &Path) -> Result<String, Vec<String>> {
    let mut json = serde_json::from_str::<Value>(&sourcemap_json)
        .map_err(|error| vec![format!("Failed to parse Rolldown source map: {error}")])?;

    if let Some(sources) = json.get_mut("sources").and_then(Value::as_array_mut) {
        for source in sources {
            if let Some(path) = source.as_str() {
                let source_path = Path::new(path);
                if let Ok(relative) = source_path.strip_prefix(cwd) {
                    *source = Value::String(relative.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }

    serde_json::to_string(&json)
        .map_err(|error| vec![format!("Failed to serialize Rolldown source map: {error}")])
}

fn inject_preamble(code: &str, preamble: &str) -> String {
    if let Some(pos) = code.find("(function() {") {
        let insert_at = pos + "(function() {".len();
        let mut result = String::with_capacity(code.len() + preamble.len() + 2);
        result.push_str(&code[..insert_at]);
        result.push('\n');
        result.push_str(preamble);
        result.push_str(&code[insert_at..]);
        result
    } else if let Some(pos) = code.find("(function () {") {
        let insert_at = pos + "(function () {".len();
        let mut result = String::with_capacity(code.len() + preamble.len() + 2);
        result.push_str(&code[..insert_at]);
        result.push('\n');
        result.push_str(preamble);
        result.push_str(&code[insert_at..]);
        result
    } else {
        format!("{preamble}\n{code}")
    }
}

fn build_bundle_options(
    cwd: &Path,
    entry_name: String,
    opts: &BundleOptions,
    external_specifiers: Vec<String>,
) -> BundlerOptions {
    BundlerOptions {
        input: Some(vec![InputItem {
            name: Some("bundle".to_string()),
            import: entry_name,
        }]),
        cwd: Some(cwd.to_path_buf()),
        external: (!external_specifiers.is_empty()).then(|| IsExternal::from(external_specifiers)),
        file: Some("bundle.js".to_string()),
        format: Some(match opts.format.as_str() {
            "esm" => OutputFormat::Esm,
            "cjs" => OutputFormat::Cjs,
            _ => OutputFormat::Iife,
        }),
        sourcemap: opts.sourcemap.then_some(SourceMapType::Hidden),
        banner: opts
            .banner
            .clone()
            .map(|banner| AddonOutputOption::String(Some(banner))),
        footer: opts
            .footer
            .clone()
            .map(|footer| AddonOutputOption::String(Some(footer))),
        define: (!opts.define.is_empty()).then(|| {
            opts.define
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        }),
        resolve: Some(build_rolldown_resolve_options()),
        transform: Some(build_rolldown_transform_options(opts)),
        treeshake: TreeshakeOptions::Boolean(false),
        minify: opts.minify.then(|| build_minify_options(opts.drop_console)),
        ..BundlerOptions::default()
    }
}

fn bundle_with_rolldown(
    files: Vec<(String, String)>,
    opts: &BundleOptions,
) -> Result<(String, Option<String>), Vec<String>> {
    if files.is_empty() {
        return Err(vec!["bundle/2 requires at least one file".to_string()]);
    }
    if opts.entry.is_empty() {
        return Err(vec!["bundle/2 requires an :entry option".to_string()]);
    }

    let entry_name = normalize_virtual_path(&opts.entry)
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .map_err(|message| vec![message])?;
    let external_specifiers = collect_external_specifiers(&files)?;
    let tempdir = TempDir::new()
        .map_err(|error| vec![format!("Failed to create temp directory: {error}")])?;
    let cwd = tempdir
        .path()
        .canonicalize()
        .unwrap_or_else(|_| tempdir.path().to_path_buf());
    let written_paths = write_virtual_project(&tempdir, &files)?;
    if !written_paths.iter().any(|path| path == &entry_name) {
        return Err(vec![format!(
            "bundle entry {entry_name:?} was not found in files"
        )]);
    }
    let options = build_bundle_options(&cwd, entry_name, opts, external_specifiers);
    let runtime = RuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| vec![format!("Failed to initialize Tokio runtime: {error}")])?;

    let mut bundler = Bundler::new(options)
        .map_err(|errors| errors.iter().map(ToString::to_string).collect::<Vec<_>>())?;

    let output = runtime
        .block_on(bundler.generate())
        .map_err(|errors| errors.iter().map(ToString::to_string).collect::<Vec<_>>())?;

    let _ = runtime.block_on(bundler.close());

    let chunk = output
        .assets
        .into_iter()
        .find_map(|asset| match asset {
            Output::Chunk(chunk) if chunk.filename == "bundle.js" => Some(chunk),
            Output::Chunk(chunk) => Some(chunk),
            Output::Asset(_) => None,
        })
        .ok_or_else(|| vec!["Rolldown did not produce a JavaScript bundle".to_string()])?;

    let mut code = chunk.code.clone();

    if let Some(preamble) = &opts.preamble {
        if !preamble.is_empty() {
            code = inject_preamble(&code, preamble);
        }
    }

    let sourcemap = if opts.sourcemap {
        let sourcemap_json = chunk
            .map
            .as_ref()
            .map(oxc_sourcemap::SourceMap::to_json_string)
            .ok_or_else(|| vec!["Rolldown did not produce a source map".to_string()])?;
        Some(relativize_sourcemap_sources(sourcemap_json, &cwd)?)
    } else {
        None
    };

    Ok((code, sourcemap))
}

#[rustler::nif(schedule = "DirtyCpu")]
fn bundle<'a>(
    env: Env<'a>,
    files: Vec<(String, String)>,
    opts_term: Term<'a>,
) -> NifResult<Term<'a>> {
    let opts = decode_options::<BundleOptions>(opts_term);

    match bundle_with_rolldown(files, &opts) {
        Ok((code, Some(sourcemap))) => encode_ok(env, CodeWithSourcemap { code, sourcemap }),
        Ok((code, None)) => encode_ok(env, code),
        Err(errors) => Ok((atoms::error(), errors).encode(env)),
    }
}

rustler::init!("Elixir.OXC.Native");
