use std::path::PathBuf;

use oxc_allocator::Allocator;
use oxc_codegen::{Codegen, CodegenOptions, CodegenReturn};
use oxc_minifier::{CompressOptions, MangleOptions, Minifier, MinifierOptions};
use oxc_parser::{ParseOptions, Parser};
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;
use oxc_transformer::{EnvOptions, JsxRuntime, TransformOptions, Transformer};
use rustler::{Encoder, Env, NifResult, SerdeTerm, Term};
use serde::Serialize;
use serde_json::Value;
use std::path::Path;

use crate::atoms;
use crate::error::{error_to_term, format_errors};
use crate::options::{decode_options, MinifyInput, TransformInput};

fn parser_options() -> ParseOptions {
    ParseOptions {
        parse_regular_expression: true,
        ..ParseOptions::default()
    }
}

fn encode_ok<'a, T: Serialize>(env: Env<'a>, value: T) -> NifResult<Term<'a>> {
    Ok((atoms::ok(), SerdeTerm(value)).encode(env))
}

#[derive(Serialize)]
struct CodeWithSourcemap {
    code: String,
    sourcemap: String,
}

pub fn build_transform_options(
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

// -- Shared transform logic used by both `transform` and `transform_many` --

pub enum TransformOutput {
    Code(String),
    CodeWithMap { code: String, sourcemap: String },
    Error(Vec<String>),
}

impl TransformOutput {
    pub fn to_term<'a>(&self, env: Env<'a>) -> Term<'a> {
        match self {
            TransformOutput::Code(code) => (atoms::ok(), code.as_str()).encode(env),
            TransformOutput::CodeWithMap { code, sourcemap } => (
                atoms::ok(),
                SerdeTerm(CodeWithSourcemap {
                    code: code.clone(),
                    sourcemap: sourcemap.clone(),
                }),
            )
                .encode(env),
            TransformOutput::Error(errors) => {
                crate::error::error_to_term(env, errors).unwrap_or_else(|_| atoms::error().encode(env))
            }
        }
    }
}

pub fn transform_source(source: &str, filename: &str, opts: &TransformInput) -> TransformOutput {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(filename).unwrap_or_default();
    let path = Path::new(filename);

    let ret = Parser::new(&allocator, source, source_type)
        .with_options(parser_options())
        .parse();

    if !ret.errors.is_empty() {
        return TransformOutput::Error(format_errors(&ret.errors));
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
        return TransformOutput::Error(format_errors(&result.errors));
    }

    if opts.sourcemap {
        let CodegenReturn { code, map, .. } = Codegen::new()
            .with_options(CodegenOptions {
                source_map_path: Some(PathBuf::from(filename)),
                ..CodegenOptions::default()
            })
            .build(&program);

        match map {
            Some(map) => TransformOutput::CodeWithMap {
                code,
                sourcemap: map.to_json_string(),
            },
            None => TransformOutput::Code(code),
        }
    } else {
        let CodegenReturn { code, .. } = Codegen::new().build(&program);
        TransformOutput::Code(code)
    }
}

// -- NIF entry points --

#[rustler::nif(schedule = "DirtyCpu")]
pub fn parse<'a>(env: Env<'a>, source: &str, filename: &str) -> NifResult<Term<'a>> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(filename).unwrap_or_default();
    let ret = Parser::new(&allocator, source, source_type)
        .with_options(parser_options())
        .parse();

    if !ret.errors.is_empty() {
        return error_to_term(env, &format_errors(&ret.errors));
    }

    let json_str = ret.program.to_estree_ts_json(false);
    let mut deserializer = serde_json::Deserializer::from_str(&json_str);
    deserializer.disable_recursion_limit();
    let json: Value = match deserializer.into_iter().next() {
        Some(Ok(v)) => v,
        Some(Err(e)) => {
            return error_to_term(
                env,
                &[format!("Failed to deserialize ESTree JSON: {e}")],
            )
        }
        None => return error_to_term(env, &["Empty ESTree JSON output".to_string()]),
    };
    encode_ok(env, json)
}

#[rustler::nif(schedule = "DirtyCpu")]
pub fn valid(source: &str, filename: &str) -> bool {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(filename).unwrap_or_default();
    let ret = Parser::new(&allocator, source, source_type).parse();
    ret.errors.is_empty()
}

#[rustler::nif(schedule = "DirtyCpu")]
pub fn transform<'a>(
    env: Env<'a>,
    source: &str,
    filename: &str,
    opts_term: Term<'a>,
) -> NifResult<Term<'a>> {
    let opts = decode_options::<TransformInput>(opts_term);
    Ok(transform_source(source, filename, &opts).to_term(env))
}

#[rustler::nif(schedule = "DirtyCpu")]
pub fn minify<'a>(
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
        return error_to_term(env, &format_errors(&ret.errors));
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
