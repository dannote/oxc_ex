use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use oxc_allocator::Allocator;
use oxc_ast::ast::{ImportOrExportKind, Program, Statement};
use oxc_codegen::{Codegen, CodegenOptions, CodegenReturn, Context, Gen};
use oxc_minifier::{CompressOptions, MangleOptions, Minifier, MinifierOptions};
use oxc_parser::{ParseOptions, Parser};
use oxc_semantic::SemanticBuilder;
use oxc_sourcemap::{ConcatSourceMapBuilder, SourceMap, SourceMapBuilder};
use oxc_span::{GetSpan, SourceType, Span};
use oxc_syntax::node::NodeId;
use oxc_transformer::{EnvOptions, JsxRuntime, TransformOptions, Transformer};
use oxc_transformer_plugins::{ReplaceGlobalDefines, ReplaceGlobalDefinesConfig};
use rustler::{Encoder, Env, NifResult, Term};
use serde_json::Value;

mod atoms {
    rustler::atoms! {
        ok,
        error,
        message,
        code,
        sourcemap,
    }
}

fn json_to_term<'a>(env: Env<'a>, value: &Value) -> Term<'a> {
    match value {
        Value::Null => rustler::types::atom::nil().encode(env),
        Value::Bool(b) => b.encode(env),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.encode(env)
            } else if let Some(f) = n.as_f64() {
                f.encode(env)
            } else {
                rustler::types::atom::nil().encode(env)
            }
        }
        Value::String(s) => s.as_str().encode(env),
        Value::Array(arr) => {
            let terms: Vec<Term<'a>> = arr.iter().map(|v| json_to_term(env, v)).collect();
            terms.encode(env)
        }
        Value::Object(map) => {
            let keys: Vec<Term<'a>> = map
                .keys()
                .map(|k| {
                    rustler::types::atom::Atom::from_str(env, k)
                        .unwrap()
                        .encode(env)
                })
                .collect();
            let vals: Vec<Term<'a>> = map.values().map(|v| json_to_term(env, v)).collect();
            Term::map_from_arrays(env, &keys, &vals).unwrap()
        }
    }
}

fn format_errors(errors: &[oxc_diagnostics::OxcDiagnostic]) -> Vec<String> {
    errors.iter().map(ToString::to_string).collect()
}

#[rustler::nif(schedule = "DirtyCpu")]
fn parse<'a>(env: Env<'a>, source: &str, filename: &str) -> NifResult<Term<'a>> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(filename).unwrap_or_default();
    let ret = Parser::new(&allocator, source, source_type)
        .with_options(ParseOptions {
            parse_regular_expression: true,
            ..ParseOptions::default()
        })
        .parse();

    if !ret.errors.is_empty() {
        let errors: Vec<Term<'a>> = ret
            .errors
            .iter()
            .map(|e| {
                let msg = e.to_string();
                Term::map_from_arrays(env, &[atoms::message().encode(env)], &[msg.encode(env)])
                    .unwrap()
            })
            .collect();
        return Ok((atoms::error(), errors).encode(env));
    }

    let json_str = ret.program.to_estree_ts_json(false);
    let json: Value = serde_json::from_str(&json_str).unwrap();
    let term = json_to_term(env, &json);

    Ok((atoms::ok(), term).encode(env))
}

#[rustler::nif(schedule = "DirtyCpu")]
fn valid(source: &str, filename: &str) -> bool {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(filename).unwrap_or_default();
    let ret = Parser::new(&allocator, source, source_type).parse();
    ret.errors.is_empty()
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
#[allow(clippy::too_many_arguments)]
fn transform<'a>(
    env: Env<'a>,
    source: &str,
    filename: &str,
    jsx_runtime: &str,
    jsx_factory: &str,
    jsx_fragment: &str,
    import_source: &str,
    target: &str,
    sourcemap: bool,
) -> NifResult<Term<'a>> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(filename).unwrap_or_default();
    let path = Path::new(filename);

    let ret = Parser::new(&allocator, source, source_type)
        .with_options(ParseOptions {
            parse_regular_expression: true,
            ..ParseOptions::default()
        })
        .parse();

    if !ret.errors.is_empty() {
        let msgs = format_errors(&ret.errors);
        return Ok((atoms::error(), msgs).encode(env));
    }

    let mut program = ret.program;
    let scoping = SemanticBuilder::new()
        .build(&program)
        .semantic
        .into_scoping();

    let options = build_transform_options(
        jsx_runtime,
        jsx_factory,
        jsx_fragment,
        import_source,
        target,
    );

    let result =
        Transformer::new(&allocator, path, &options).build_with_scoping(scoping, &mut program);

    if !result.errors.is_empty() {
        let msgs = format_errors(&result.errors);
        return Ok((atoms::error(), msgs).encode(env));
    }

    if sourcemap {
        let codegen_opts = CodegenOptions {
            source_map_path: Some(PathBuf::from(filename)),
            ..Default::default()
        };
        let CodegenReturn { code, map, .. } =
            Codegen::new().with_options(codegen_opts).build(&program);
        if let Some(map) = map {
            let map_json = map.to_json_string();
            let result = Term::map_from_arrays(
                env,
                &[atoms::code().encode(env), atoms::sourcemap().encode(env)],
                &[code.encode(env), map_json.encode(env)],
            )
            .unwrap();
            Ok((atoms::ok(), result).encode(env))
        } else {
            Ok((atoms::ok(), code).encode(env))
        }
    } else {
        let CodegenReturn { code, .. } = Codegen::new().build(&program);
        Ok((atoms::ok(), code).encode(env))
    }
}

#[rustler::nif(schedule = "DirtyCpu")]
fn minify<'a>(env: Env<'a>, source: &str, filename: &str, mangle: bool) -> NifResult<Term<'a>> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(filename).unwrap_or_default();

    let ret = Parser::new(&allocator, source, source_type)
        .with_options(ParseOptions {
            parse_regular_expression: true,
            ..ParseOptions::default()
        })
        .parse();

    if !ret.errors.is_empty() {
        let msgs = format_errors(&ret.errors);
        return Ok((atoms::error(), msgs).encode(env));
    }

    let mut program = ret.program;

    let options = MinifierOptions {
        mangle: mangle.then(MangleOptions::default),
        compress: Some(CompressOptions::default()),
    };
    let min_ret = Minifier::new(options).minify(&allocator, &mut program);

    let CodegenReturn { code, .. } = Codegen::new()
        .with_options(CodegenOptions::minify())
        .with_scoping(min_ret.scoping)
        .build(&program);

    Ok((atoms::ok(), code).encode(env))
}

#[rustler::nif(schedule = "DirtyCpu")]
fn imports<'a>(env: Env<'a>, source: &str, filename: &str) -> NifResult<Term<'a>> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(filename).unwrap_or_default();
    let ret = Parser::new(&allocator, source, source_type)
        .with_options(ParseOptions {
            parse_regular_expression: true,
            ..ParseOptions::default()
        })
        .parse();

    if !ret.errors.is_empty() {
        let msgs = format_errors(&ret.errors);
        return Ok((atoms::error(), msgs).encode(env));
    }

    let mut specifiers = Vec::new();
    for stmt in ret.program.body.iter() {
        if let Statement::ImportDeclaration(decl) = stmt {
            if decl.import_kind != ImportOrExportKind::Type {
                specifiers.push(decl.source.value.to_string());
            }
        }
    }

    Ok((atoms::ok(), specifiers).encode(env))
}

/// Normalize a virtual module path or specifier to a stable lookup key.
fn normalize_module_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let mut parts = Vec::new();

    for component in Path::new(&normalized).components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                parts.pop();
            }
            std::path::Component::Normal(part) => {
                parts.push(part.to_string_lossy().into_owned());
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {}
        }
    }

    let joined = parts.join("/");
    joined
        .strip_suffix(".ts")
        .or_else(|| joined.strip_suffix(".tsx"))
        .or_else(|| joined.strip_suffix(".js"))
        .or_else(|| joined.strip_suffix(".jsx"))
        .unwrap_or(&joined)
        .to_string()
}

fn module_var(index: usize) -> String {
    format!("__oxc_bundle_module_{index}")
}

fn js_string(value: &str) -> String {
    serde_json::to_string(value).unwrap()
}

fn append_code(target: &mut String, code: &str) {
    target.push_str(code);
    if !code.ends_with('\n') {
        target.push('\n');
    }
}

fn count_lines(code: &str) -> u32 {
    code.bytes().filter(|byte| *byte == b'\n').count() as u32
}

struct GeneratedChunk {
    code: String,
    sourcemap: Option<SourceMap>,
}

fn generate_statement_chunk<'a>(
    allocator: &'a Allocator,
    source_text: &'a str,
    filename: &str,
    source_type: SourceType,
    stmt: Statement<'a>,
    sourcemap: bool,
) -> GeneratedChunk {
    let mut body = oxc_allocator::Vec::new_in(allocator);
    body.push(stmt);

    let program = Program {
        node_id: Cell::new(NodeId::DUMMY),
        span: Span::new(0, source_text.len() as u32),
        source_type,
        source_text,
        comments: oxc_allocator::Vec::new_in(allocator),
        hashbang: None,
        directives: oxc_allocator::Vec::new_in(allocator),
        body,
        scope_id: Cell::new(None),
    };

    if sourcemap {
        let codegen_opts = CodegenOptions {
            source_map_path: Some(PathBuf::from(filename)),
            ..Default::default()
        };
        let CodegenReturn { code, map, .. } = Codegen::new()
            .with_options(codegen_opts)
            .with_source_text(source_text)
            .build(&program);
        GeneratedChunk {
            code,
            sourcemap: map,
        }
    } else {
        let CodegenReturn { code, .. } = Codegen::new().build(&program);
        GeneratedChunk {
            code,
            sourcemap: None,
        }
    }
}

fn line_col_from_offset(source_text: &str, offset: u32) -> (u32, u32) {
    let mut line = 0;
    let mut col = 0;

    for byte in source_text.as_bytes().iter().take(offset as usize) {
        if *byte == b'\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }

    (line, col)
}

fn synthetic_chunk_with_map(
    filename: &str,
    source_text: &str,
    code: String,
    generated_col: u32,
    source_span: Span,
    sourcemap: bool,
) -> GeneratedChunk {
    if !sourcemap {
        return GeneratedChunk {
            code,
            sourcemap: None,
        };
    }

    let (src_line, src_col) = line_col_from_offset(source_text, source_span.start);
    let mut builder = SourceMapBuilder::default();
    builder.set_file(filename);
    let source_id = builder.set_source_and_content(filename, source_text);
    builder.add_token(0, generated_col, src_line, src_col, Some(source_id), None);

    GeneratedChunk {
        code,
        sourcemap: Some(builder.into_sourcemap()),
    }
}

fn concat_sourcemaps_with_offsets(entries: &[(SourceMap, u32)], file: &str) -> Option<SourceMap> {
    if entries.is_empty() {
        return None;
    }

    let refs: Vec<(&SourceMap, u32)> = entries.iter().map(|(map, offset)| (map, *offset)).collect();
    let mut sourcemap = ConcatSourceMapBuilder::from_sourcemaps(&refs).into_sourcemap();
    sourcemap.set_file(file);
    Some(sourcemap)
}

fn compose_sourcemaps(remapped: &SourceMap, original: &SourceMap, file: &str) -> SourceMap {
    let lookup_table = original.generate_lookup_table();
    let mut builder = SourceMapBuilder::default();
    builder.set_file(file);

    for token in remapped.get_tokens() {
        if let Some(source_token) = original.lookup_source_view_token(
            &lookup_table,
            token.get_src_line(),
            token.get_src_col(),
        ) {
            let source_id = source_token.get_source().map(|source| {
                let content = source_token
                    .get_source_content()
                    .map(|content| content.as_ref())
                    .unwrap_or("");
                builder.add_source_and_content(source.as_ref(), content)
            });
            let name_id = source_token
                .get_name()
                .map(|name| builder.add_name(name.as_ref()));

            builder.add_token(
                token.get_dst_line(),
                token.get_dst_col(),
                source_token.get_src_line(),
                source_token.get_src_col(),
                source_id,
                name_id,
            );
        }
    }

    builder.into_sourcemap()
}

struct ResolvedModule {
    dependency: Option<usize>,
    expr: String,
}

fn resolve_module_reference(
    importer_id: &str,
    specifier: &str,
    module_indices: &HashMap<String, usize>,
    module_vars: &[String],
) -> Result<ResolvedModule, String> {
    let importer_dir = Path::new(importer_id)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let resolved = if specifier.starts_with('.') {
        importer_dir.join(specifier)
    } else {
        PathBuf::from(specifier)
    };
    let key = normalize_module_path(&resolved.to_string_lossy());

    if let Some(index) = module_indices.get(&key).copied() {
        Ok(ResolvedModule {
            dependency: Some(index),
            expr: module_vars[index].clone(),
        })
    } else if specifier.starts_with('.') {
        Err(format!(
            "Failed to resolve module specifier {specifier:?} from {importer_id:?}"
        ))
    } else {
        Ok(ResolvedModule {
            dependency: None,
            expr: format!("(globalThis[{}] ?? {{}})", js_string(specifier)),
        })
    }
}

fn collect_binding_names(pattern: &oxc_ast::ast::BindingPattern<'_>, names: &mut Vec<String>) {
    match pattern {
        oxc_ast::ast::BindingPattern::BindingIdentifier(ident) => {
            names.push(ident.name.as_str().to_string());
        }
        oxc_ast::ast::BindingPattern::ObjectPattern(pattern) => {
            for property in &pattern.properties {
                collect_binding_names(&property.value, names);
            }
            if let Some(rest) = &pattern.rest {
                collect_binding_names(&rest.argument, names);
            }
        }
        oxc_ast::ast::BindingPattern::ArrayPattern(pattern) => {
            for element in pattern.elements.iter().flatten() {
                collect_binding_names(element, names);
            }
            if let Some(rest) = &pattern.rest {
                collect_binding_names(&rest.argument, names);
            }
        }
        oxc_ast::ast::BindingPattern::AssignmentPattern(pattern) => {
            collect_binding_names(&pattern.left, names);
        }
    }
}

fn declared_binding_names(declaration: &oxc_ast::ast::Declaration<'_>) -> Vec<String> {
    let mut names = Vec::new();

    match declaration {
        oxc_ast::ast::Declaration::VariableDeclaration(decl) => {
            for declarator in &decl.declarations {
                collect_binding_names(&declarator.id, &mut names);
            }
        }
        oxc_ast::ast::Declaration::FunctionDeclaration(decl) => {
            if let Some(id) = &decl.id {
                names.push(id.name.as_str().to_string());
            }
        }
        oxc_ast::ast::Declaration::ClassDeclaration(decl) => {
            if let Some(id) = &decl.id {
                names.push(id.name.as_str().to_string());
            }
        }
        _ => {}
    }

    names
}

fn render_default_export<'a>(
    allocator: &'a Allocator,
    source_text: &'a str,
    filename: &str,
    source_type: SourceType,
    module_var: &str,
    declaration: oxc_ast::ast::ExportDefaultDeclarationKind<'a>,
    sourcemap: bool,
) -> GeneratedChunk {
    match declaration {
        oxc_ast::ast::ExportDefaultDeclarationKind::FunctionDeclaration(function) => {
            if let Some(name) = function.id.as_ref().map(|id| id.name.as_str().to_string()) {
                let mut chunk = generate_statement_chunk(
                    allocator,
                    source_text,
                    filename,
                    source_type,
                    Statement::FunctionDeclaration(function),
                    sourcemap,
                );
                append_code(
                    &mut chunk.code,
                    &format!("{module_var}[\"default\"] = {name};"),
                );
                chunk
            } else {
                let mut function = function;
                let source_span = function.span;
                function.r#type = oxc_ast::ast::FunctionType::FunctionExpression;
                let prefix = format!("{module_var}[\"default\"] = ");
                let generated_col = prefix.len() as u32;
                let mut codegen = Codegen::new();
                codegen.print_str(&prefix);
                function.print(&mut codegen, Context::default());
                codegen.print_str(";");
                let mut code = codegen.into_source_text();
                code.push('\n');
                synthetic_chunk_with_map(
                    filename,
                    source_text,
                    code,
                    generated_col,
                    source_span,
                    sourcemap,
                )
            }
        }
        oxc_ast::ast::ExportDefaultDeclarationKind::ClassDeclaration(class) => {
            if let Some(name) = class.id.as_ref().map(|id| id.name.as_str().to_string()) {
                let mut chunk = generate_statement_chunk(
                    allocator,
                    source_text,
                    filename,
                    source_type,
                    Statement::ClassDeclaration(class),
                    sourcemap,
                );
                append_code(
                    &mut chunk.code,
                    &format!("{module_var}[\"default\"] = {name};"),
                );
                chunk
            } else {
                let mut class = class;
                let source_span = class.span;
                class.r#type = oxc_ast::ast::ClassType::ClassExpression;
                let prefix = format!("{module_var}[\"default\"] = ");
                let generated_col = prefix.len() as u32;
                let mut codegen = Codegen::new();
                codegen.print_str(&prefix);
                class.print(&mut codegen, Context::default());
                codegen.print_str(";");
                let mut code = codegen.into_source_text();
                code.push('\n');
                synthetic_chunk_with_map(
                    filename,
                    source_text,
                    code,
                    generated_col,
                    source_span,
                    sourcemap,
                )
            }
        }
        oxc_ast::ast::ExportDefaultDeclarationKind::TSInterfaceDeclaration(_) => GeneratedChunk {
            code: String::new(),
            sourcemap: None,
        },
        expression => {
            let expression = expression.into_expression().into_inner_expression();
            let source_span = expression.span();
            let prefix = format!("{module_var}[\"default\"] = ");
            let generated_col = prefix.len() as u32;
            let mut codegen = Codegen::new();
            codegen.print_str(&prefix);
            codegen.print_expression(&expression);
            codegen.print_str(";");
            let mut code = codegen.into_source_text();
            code.push('\n');
            synthetic_chunk_with_map(
                filename,
                source_text,
                code,
                generated_col,
                source_span,
                sourcemap,
            )
        }
    }
}

fn push_dependency(dependencies: &mut Vec<usize>, seen: &mut HashSet<usize>, dependency: usize) {
    if seen.insert(dependency) {
        dependencies.push(dependency);
    }
}

/// Transform a single TS/JS module into an isolated wrapper body plus its dependencies.
#[allow(clippy::too_many_arguments)]
fn transform_module(
    allocator: &Allocator,
    source: &str,
    filename: &str,
    module_id: &str,
    module_index: usize,
    module_vars: &[String],
    module_indices: &HashMap<String, usize>,
    transform_options: &TransformOptions,
    sourcemap: bool,
) -> Result<(String, Vec<usize>, Option<SourceMap>), Vec<String>> {
    let source_type = SourceType::from_path(filename).unwrap_or_default();
    let path = Path::new(filename);

    let ret = Parser::new(allocator, source, source_type)
        .with_options(ParseOptions {
            parse_regular_expression: true,
            ..ParseOptions::default()
        })
        .parse();

    if !ret.errors.is_empty() {
        return Err(format_errors(&ret.errors));
    }

    let mut program = ret.program;

    let scoping = SemanticBuilder::new()
        .build(&program)
        .semantic
        .into_scoping();

    let result = Transformer::new(allocator, path, transform_options)
        .build_with_scoping(scoping, &mut program);

    if !result.errors.is_empty() {
        return Err(format_errors(&result.errors));
    }

    let module_var = &module_vars[module_index];
    let mut prelude = String::new();
    let mut prelude_line_count = 0u32;
    let mut body = String::new();
    let mut body_line_count = 0u32;
    let mut body_maps = Vec::new();
    let mut dependencies = Vec::new();
    let mut seen_dependencies = HashSet::new();

    for stmt in program.body.into_iter() {
        match stmt {
            Statement::ImportDeclaration(decl) => {
                if decl.import_kind == ImportOrExportKind::Type {
                    continue;
                }

                let dependency = resolve_module_reference(
                    module_id,
                    decl.source.value.as_str(),
                    module_indices,
                    module_vars,
                )
                .map_err(|msg| vec![msg])?;
                if let Some(index) = dependency.dependency {
                    push_dependency(&mut dependencies, &mut seen_dependencies, index);
                }
                let dependency_var = &dependency.expr;

                if let Some(specifiers) = &decl.specifiers {
                    for specifier in specifiers {
                        let lines = match specifier {
                            oxc_ast::ast::ImportDeclarationSpecifier::ImportSpecifier(spec) => {
                                if spec.import_kind == ImportOrExportKind::Type {
                                    0
                                } else {
                                    append_code(
                                        &mut prelude,
                                        &format!(
                                            "const {} = {}[{}];",
                                            spec.local.name.as_str(),
                                            dependency_var,
                                            js_string(spec.imported.name().as_str())
                                        ),
                                    );
                                    1
                                }
                            }
                            oxc_ast::ast::ImportDeclarationSpecifier::ImportDefaultSpecifier(
                                spec,
                            ) => {
                                append_code(
                                    &mut prelude,
                                    &format!(
                                        "const {} = {}[\"default\"];",
                                        spec.local.name.as_str(),
                                        dependency_var
                                    ),
                                );
                                1
                            }
                            oxc_ast::ast::ImportDeclarationSpecifier::ImportNamespaceSpecifier(
                                spec,
                            ) => {
                                append_code(
                                    &mut prelude,
                                    &format!(
                                        "const {} = {};",
                                        spec.local.name.as_str(),
                                        dependency_var
                                    ),
                                );
                                1
                            }
                        };
                        prelude_line_count += lines;
                    }
                }
            }
            Statement::ExportAllDeclaration(decl) => {
                if decl.export_kind == ImportOrExportKind::Type {
                    continue;
                }

                let dependency = resolve_module_reference(
                    module_id,
                    decl.source.value.as_str(),
                    module_indices,
                    module_vars,
                )
                .map_err(|msg| vec![msg])?;
                if let Some(index) = dependency.dependency {
                    push_dependency(&mut dependencies, &mut seen_dependencies, index);
                }
                let dependency_var = &dependency.expr;

                let code = if let Some(exported) = &decl.exported {
                    format!(
                        "{}[{}] = {};",
                        module_var,
                        js_string(exported.name().as_str()),
                        dependency_var
                    )
                } else {
                    format!(
                        "{{\nfor (const [__oxc_bundle_key, __oxc_bundle_value] of Object.entries({})) {{\nif (__oxc_bundle_key !== \"default\") {}[__oxc_bundle_key] = __oxc_bundle_value;\n}}\n}}",
                        dependency_var, module_var
                    )
                };
                append_code(&mut body, &code);
                body_line_count += count_lines(&code).max(1);
            }
            Statement::ExportNamedDeclaration(decl) => {
                let inner = decl.unbox();

                if inner.export_kind == ImportOrExportKind::Type {
                    continue;
                }

                if let Some(source) = &inner.source {
                    let dependency = resolve_module_reference(
                        module_id,
                        source.value.as_str(),
                        module_indices,
                        module_vars,
                    )
                    .map_err(|msg| vec![msg])?;
                    if let Some(index) = dependency.dependency {
                        push_dependency(&mut dependencies, &mut seen_dependencies, index);
                    }
                    let dependency_var = &dependency.expr;

                    for specifier in inner.specifiers.iter() {
                        if specifier.export_kind == ImportOrExportKind::Type {
                            continue;
                        }

                        let code = format!(
                            "{}[{}] = {}[{}];",
                            module_var,
                            js_string(specifier.exported.name().as_str()),
                            dependency_var,
                            js_string(specifier.local.name().as_str())
                        );
                        append_code(&mut body, &code);
                        body_line_count += count_lines(&code).max(1);
                    }

                    continue;
                }

                if let Some(declaration) = inner.declaration {
                    let exported_names = declared_binding_names(&declaration);
                    let chunk = generate_statement_chunk(
                        allocator,
                        source,
                        filename,
                        source_type,
                        Statement::from(declaration),
                        sourcemap,
                    );
                    if let Some(map) = chunk.sourcemap {
                        body_maps.push((map, body_line_count));
                    }
                    body_line_count += if chunk.code.is_empty() {
                        0
                    } else {
                        count_lines(&chunk.code).max(1)
                    };
                    body.push_str(&chunk.code);
                    if !chunk.code.ends_with('\n') {
                        body.push('\n');
                    }

                    for exported_name in exported_names {
                        let code = format!(
                            "{}[{}] = {};",
                            module_var,
                            js_string(&exported_name),
                            exported_name
                        );
                        append_code(&mut body, &code);
                        body_line_count += count_lines(&code).max(1);
                    }
                }

                for specifier in inner.specifiers.iter() {
                    if specifier.export_kind == ImportOrExportKind::Type {
                        continue;
                    }

                    let code = format!(
                        "{}[{}] = {};",
                        module_var,
                        js_string(specifier.exported.name().as_str()),
                        specifier.local.name().as_str()
                    );
                    append_code(&mut body, &code);
                    body_line_count += count_lines(&code).max(1);
                }
            }
            Statement::ExportDefaultDeclaration(decl) => {
                let declaration = decl.unbox().declaration;
                let chunk = render_default_export(
                    allocator,
                    source,
                    filename,
                    source_type,
                    module_var,
                    declaration,
                    sourcemap,
                );
                if let Some(map) = chunk.sourcemap {
                    body_maps.push((map, body_line_count));
                }
                body_line_count += if chunk.code.is_empty() {
                    0
                } else {
                    count_lines(&chunk.code).max(1)
                };
                body.push_str(&chunk.code);
                if !chunk.code.ends_with('\n') {
                    body.push('\n');
                }
            }
            other => {
                let chunk = generate_statement_chunk(
                    allocator,
                    source,
                    filename,
                    source_type,
                    other,
                    sourcemap,
                );
                if let Some(map) = chunk.sourcemap {
                    body_maps.push((map, body_line_count));
                }
                body_line_count += if chunk.code.is_empty() {
                    0
                } else {
                    count_lines(&chunk.code).max(1)
                };
                body.push_str(&chunk.code);
                if !chunk.code.ends_with('\n') {
                    body.push('\n');
                }
            }
        }
    }

    let mut result = String::new();
    result.push_str("(() => {\n");
    result.push_str(&prelude);
    let body_base_offset =
        1 + prelude_line_count + u32::from(!prelude.is_empty() && !body.is_empty());
    if !prelude.is_empty() && !body.is_empty() {
        result.push('\n');
    }
    result.push_str(&body);
    result.push_str("})();\n");

    let module_maps = body_maps
        .into_iter()
        .map(|(map, offset)| (map, body_base_offset + offset))
        .collect::<Vec<_>>();
    let sourcemap = concat_sourcemaps_with_offsets(&module_maps, filename);

    Ok((result, dependencies, sourcemap))
}

/// Topologically sort modules by their import dependencies (Kahn's algorithm).
fn topo_sort(dependencies: &[Vec<usize>]) -> Vec<usize> {
    let mut in_degree = vec![0usize; dependencies.len()];
    let mut dependents = vec![Vec::new(); dependencies.len()];

    for (index, deps) in dependencies.iter().enumerate() {
        for &dependency in deps {
            in_degree[index] += 1;
            dependents[dependency].push(index);
        }
    }

    let mut queue = VecDeque::new();
    for (index, degree) in in_degree.iter().enumerate() {
        if *degree == 0 {
            queue.push_back(index);
        }
    }

    let mut sorted = Vec::new();
    while let Some(index) = queue.pop_front() {
        sorted.push(index);
        for &dependent in &dependents[index] {
            in_degree[dependent] -= 1;
            if in_degree[dependent] == 0 {
                queue.push_back(dependent);
            }
        }
    }

    if sorted.len() != dependencies.len() {
        let seen: HashSet<usize> = sorted.iter().copied().collect();
        for index in 0..dependencies.len() {
            if !seen.contains(&index) {
                sorted.push(index);
            }
        }
    }

    sorted
}

/// Decoded bundle options from Elixir keyword list.
struct BundleOptions {
    minify: bool,
    banner: Option<String>,
    footer: Option<String>,
    define: Vec<(String, String)>,
    sourcemap: bool,
    drop_console: bool,
    jsx_runtime: String,
    jsx_factory: String,
    jsx_fragment: String,
    import_source: String,
    target: String,
}

impl BundleOptions {
    fn from_term(env: Env<'_>, term: Term<'_>) -> Self {
        let mut opts = Self {
            minify: false,
            banner: None,
            footer: None,
            define: Vec::new(),
            sourcemap: false,
            drop_console: false,
            jsx_runtime: "automatic".to_string(),
            jsx_factory: String::new(),
            jsx_fragment: String::new(),
            import_source: String::new(),
            target: String::new(),
        };

        let minify_atom = rustler::types::atom::Atom::from_str(env, "minify").unwrap();
        let banner_atom = rustler::types::atom::Atom::from_str(env, "banner").unwrap();
        let footer_atom = rustler::types::atom::Atom::from_str(env, "footer").unwrap();
        let sourcemap_atom = rustler::types::atom::Atom::from_str(env, "sourcemap").unwrap();
        let drop_console_atom = rustler::types::atom::Atom::from_str(env, "drop_console").unwrap();
        let define_atom = rustler::types::atom::Atom::from_str(env, "define").unwrap();
        let jsx_atom = rustler::types::atom::Atom::from_str(env, "jsx").unwrap();
        let jsx_factory_atom = rustler::types::atom::Atom::from_str(env, "jsx_factory").unwrap();
        let jsx_fragment_atom = rustler::types::atom::Atom::from_str(env, "jsx_fragment").unwrap();
        let import_source_atom =
            rustler::types::atom::Atom::from_str(env, "import_source").unwrap();
        let target_atom = rustler::types::atom::Atom::from_str(env, "target").unwrap();

        if let Ok(list) = term.decode::<Vec<(rustler::Atom, Term<'_>)>>() {
            for (key, val) in list {
                if key == minify_atom {
                    opts.minify = val.decode::<bool>().unwrap_or(false);
                } else if key == banner_atom {
                    opts.banner = val.decode::<String>().ok();
                } else if key == footer_atom {
                    opts.footer = val.decode::<String>().ok();
                } else if key == sourcemap_atom {
                    opts.sourcemap = val.decode::<bool>().unwrap_or(false);
                } else if key == drop_console_atom {
                    opts.drop_console = val.decode::<bool>().unwrap_or(false);
                } else if key == jsx_atom {
                    let classic = rustler::types::atom::Atom::from_str(env, "classic").unwrap();
                    if let Ok(atom) = val.decode::<rustler::Atom>() {
                        if atom == classic {
                            opts.jsx_runtime = "classic".to_string();
                        }
                    }
                } else if key == jsx_factory_atom {
                    opts.jsx_factory = val.decode::<String>().unwrap_or_default();
                } else if key == jsx_fragment_atom {
                    opts.jsx_fragment = val.decode::<String>().unwrap_or_default();
                } else if key == define_atom {
                    if let Ok(map) = val.decode::<HashMap<String, String>>() {
                        opts.define = map.into_iter().collect();
                    }
                } else if key == import_source_atom {
                    opts.import_source = val.decode::<String>().unwrap_or_default();
                } else if key == target_atom {
                    opts.target = val.decode::<String>().unwrap_or_default();
                }
            }
        }

        opts
    }
}

#[rustler::nif(schedule = "DirtyCpu")]
fn bundle<'a>(
    env: Env<'a>,
    files: Vec<(String, String)>,
    opts_term: Term<'a>,
) -> NifResult<Term<'a>> {
    let opts = BundleOptions::from_term(env, opts_term);

    let mut module_indices = HashMap::new();
    let mut module_ids = Vec::with_capacity(files.len());
    for (index, (filename, _)) in files.iter().enumerate() {
        let module_id = normalize_module_path(filename);
        if let Some(previous) = module_indices.insert(module_id.clone(), index) {
            let previous_filename = &files[previous].0;
            return Ok((
                atoms::error(),
                vec![format!(
                    "Duplicate module path after normalization: {filename:?} conflicts with {previous_filename:?}"
                )],
            )
                .encode(env));
        }
        module_ids.push(module_id);
    }

    let module_vars: Vec<String> = (0..files.len()).map(module_var).collect();

    let transform_options = build_transform_options(
        &opts.jsx_runtime,
        &opts.jsx_factory,
        &opts.jsx_fragment,
        &opts.import_source,
        &opts.target,
    );

    let mut wrapped_modules = vec![String::new(); files.len()];
    let mut module_maps = vec![None; files.len()];
    let mut dependencies = vec![Vec::new(); files.len()];

    for (index, ((filename, source), module_id)) in files.iter().zip(&module_ids).enumerate() {
        let allocator = Allocator::default();
        match transform_module(
            &allocator,
            source,
            filename,
            module_id,
            index,
            &module_vars,
            &module_indices,
            &transform_options,
            opts.sourcemap,
        ) {
            Ok((code, deps, sourcemap)) => {
                wrapped_modules[index] = code;
                module_maps[index] = sourcemap;
                dependencies[index] = deps;
            }
            Err(errors) => {
                return Ok((atoms::error(), errors).encode(env));
            }
        }
    }

    let order = topo_sort(&dependencies);

    let mut output = String::new();
    let mut bundle_line_count = 0u32;
    let mut bundle_map_entries = Vec::new();

    if let Some(ref banner) = opts.banner {
        output.push_str(banner);
        output.push('\n');
        bundle_line_count += 1;
    }
    output.push_str("(() => {\n");
    bundle_line_count += 1;
    for module_var in &module_vars {
        output.push_str(&format!("const {module_var} = {{}};\n"));
        bundle_line_count += 1;
    }
    if !module_vars.is_empty() {
        output.push('\n');
        bundle_line_count += 1;
    }
    for index in order {
        if let Some(map) = module_maps[index].take() {
            bundle_map_entries.push((map, bundle_line_count));
        }
        output.push_str(&wrapped_modules[index]);
        bundle_line_count += count_lines(&wrapped_modules[index]);
        output.push('\n');
        bundle_line_count += 1;
    }
    output.push_str("})();\n");
    if let Some(ref footer) = opts.footer {
        output.push_str(footer);
        output.push('\n');
    }

    let mut bundle_map = if opts.sourcemap {
        concat_sourcemaps_with_offsets(&bundle_map_entries, "bundle.js")
    } else {
        None
    };

    if !opts.define.is_empty() {
        let allocator = Allocator::default();
        let source_type = SourceType::default();
        let ret = Parser::new(&allocator, &output, source_type)
            .with_options(ParseOptions {
                parse_regular_expression: true,
                ..ParseOptions::default()
            })
            .parse();

        if ret.errors.is_empty() {
            let mut program = ret.program;
            let scoping = SemanticBuilder::new()
                .build(&program)
                .semantic
                .into_scoping();

            let define_pairs: Vec<(&str, &str)> = opts
                .define
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();

            if let Ok(config) = ReplaceGlobalDefinesConfig::new(&define_pairs) {
                let _ = ReplaceGlobalDefines::new(&allocator, config).build(scoping, &mut program);
                if opts.sourcemap {
                    let codegen_opts = CodegenOptions {
                        source_map_path: Some(PathBuf::from("bundle.js")),
                        ..CodegenOptions::default()
                    };
                    let CodegenReturn { code, map, .. } = Codegen::new()
                        .with_options(codegen_opts)
                        .with_source_text(&output)
                        .build(&program);
                    output = code;
                    if let Some(stage_map) = map {
                        bundle_map = Some(if let Some(previous_map) = bundle_map.take() {
                            compose_sourcemaps(&stage_map, &previous_map, "bundle.js")
                        } else {
                            stage_map
                        });
                    }
                } else {
                    let CodegenReturn { code, .. } = Codegen::new().build(&program);
                    output = code;
                }
            }
        }
    }

    let mut source_map: Option<String> = bundle_map.as_ref().map(SourceMap::to_json_string);
    if opts.minify {
        let allocator = Allocator::default();
        let source_type = SourceType::script();
        let ret = Parser::new(&allocator, &output, source_type)
            .with_options(ParseOptions {
                parse_regular_expression: true,
                ..ParseOptions::default()
            })
            .parse();

        if !ret.errors.is_empty() {
            let msgs = format_errors(&ret.errors);
            return Ok((atoms::error(), msgs).encode(env));
        }

        let mut program = ret.program;
        let mut compress = CompressOptions::default();
        if opts.drop_console {
            compress.drop_console = true;
        }
        let options = MinifierOptions {
            mangle: Some(MangleOptions::default()),
            compress: Some(compress),
        };
        let min_ret = Minifier::new(options).minify(&allocator, &mut program);

        let mut codegen_opts = CodegenOptions::minify();
        if opts.sourcemap {
            codegen_opts.source_map_path = Some(PathBuf::from("bundle.js"));
        }
        let CodegenReturn { code, map, .. } = Codegen::new()
            .with_options(codegen_opts)
            .with_source_text(&output)
            .with_scoping(min_ret.scoping)
            .build(&program);
        output = code;
        if let Some(stage_map) = map {
            if let Some(previous_map) = bundle_map.take() {
                source_map = Some(
                    compose_sourcemaps(&stage_map, &previous_map, "bundle.js").to_json_string(),
                );
            } else {
                source_map = Some(stage_map.to_json_string());
            }
        }
    }

    if let Some(ref map_json) = source_map {
        let result = Term::map_from_arrays(
            env,
            &[atoms::code().encode(env), atoms::sourcemap().encode(env)],
            &[output.encode(env), map_json.encode(env)],
        )
        .unwrap();
        Ok((atoms::ok(), result).encode(env))
    } else {
        Ok((atoms::ok(), output).encode(env))
    }
}

rustler::init!("Elixir.OXC.Native");
