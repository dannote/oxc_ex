use oxc_allocator::Allocator;
use oxc_ast::ast::{Expression, ImportOrExportKind, Statement};
use oxc_ast_visit::walk;
use oxc_ast_visit::Visit;
use oxc_parser::Parser;
use oxc_span::SourceType;
use rustler::{Encoder, Env, NifMap, NifResult, Term};

use crate::atoms;
use crate::error::{error_to_term, format_errors};

#[derive(NifMap)]
struct ImportInfo {
    specifier: String,
    r#type: rustler::Atom,
    kind: rustler::Atom,
    start: u32,
    r#end: u32,
}

struct ImportCollector {
    imports: Vec<ImportInfo>,
}

impl<'a> Visit<'a> for ImportCollector {
    fn visit_import_declaration(&mut self, decl: &oxc_ast::ast::ImportDeclaration<'a>) {
        if decl.import_kind != ImportOrExportKind::Type {
            self.imports.push(ImportInfo {
                specifier: decl.source.value.to_string(),
                r#type: atoms::atom_static(),
                kind: atoms::import(),
                start: decl.source.span.start,
                r#end: decl.source.span.end,
            });
        }
    }

    fn visit_export_named_declaration(&mut self, decl: &oxc_ast::ast::ExportNamedDeclaration<'a>) {
        if decl.export_kind != ImportOrExportKind::Type {
            if let Some(source) = &decl.source {
                self.imports.push(ImportInfo {
                    specifier: source.value.to_string(),
                    r#type: atoms::atom_static(),
                    kind: atoms::export(),
                    start: source.span.start,
                    r#end: source.span.end,
                });
            }
        }
        walk::walk_export_named_declaration(self, decl);
    }

    fn visit_export_all_declaration(&mut self, decl: &oxc_ast::ast::ExportAllDeclaration<'a>) {
        if decl.export_kind != ImportOrExportKind::Type {
            self.imports.push(ImportInfo {
                specifier: decl.source.value.to_string(),
                r#type: atoms::atom_static(),
                kind: atoms::export_all(),
                start: decl.source.span.start,
                r#end: decl.source.span.end,
            });
        }
    }

    fn visit_import_expression(&mut self, expr: &oxc_ast::ast::ImportExpression<'a>) {
        if let Expression::StringLiteral(lit) = &expr.source {
            self.imports.push(ImportInfo {
                specifier: lit.value.to_string(),
                r#type: atoms::dynamic(),
                kind: atoms::import(),
                start: lit.span.start,
                r#end: lit.span.end,
            });
        }
        walk::walk_import_expression(self, expr);
    }
}

#[rustler::nif(schedule = "DirtyCpu")]
pub fn imports<'a>(env: Env<'a>, source: &str, filename: &str) -> NifResult<Term<'a>> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(filename).unwrap_or_default();
    let ret = Parser::new(&allocator, source, source_type).parse();

    if !ret.errors.is_empty() {
        return error_to_term(env, &format_errors(&ret.errors));
    }

    let specifiers: Vec<String> = ret
        .program
        .body
        .iter()
        .filter_map(|stmt| match stmt {
            Statement::ImportDeclaration(decl) if decl.import_kind != ImportOrExportKind::Type => {
                Some(decl.source.value.to_string())
            }
            _ => None,
        })
        .collect();

    Ok((atoms::ok(), rustler::SerdeTerm(specifiers)).encode(env))
}

#[rustler::nif(schedule = "DirtyCpu")]
pub fn collect_imports<'a>(env: Env<'a>, source: &str, filename: &str) -> NifResult<Term<'a>> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(filename).unwrap_or_default();
    let ret = Parser::new(&allocator, source, source_type).parse();

    if !ret.errors.is_empty() {
        return error_to_term(env, &format_errors(&ret.errors));
    }

    let mut collector = ImportCollector {
        imports: Vec::new(),
    };
    collector.visit_program(&ret.program);

    Ok((atoms::ok(), collector.imports).encode(env))
}
