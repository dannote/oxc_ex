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
pub fn parse<'a>(env: Env<'a>, source: &str, filename: &str) -> NifResult<Term<'a>> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(filename).unwrap_or_default();
    let ret = Parser::new(&allocator, source, source_type)
        .with_options(parser_options())
        .parse();

    if !ret.errors.is_empty() {
        return error_to_term(env, &format_errors(&ret.errors));
    }

    // OXC's ESTree serializer outputs JSON strings via a custom compact serializer
    // (not standard serde::Serialize). Parsing back to Value is unavoidable without
    // writing a custom OXC-to-BEAM-term serializer.
    let json_str = ret.program.to_estree_ts_json(false);
    let mut deserializer = serde_json::Deserializer::from_str(&json_str);
    deserializer.disable_recursion_limit();
    let json: Value = match deserializer.into_iter().next() {
        Some(Ok(v)) => v,
        Some(Err(e)) => return error_to_term(env, &[format!("Failed to deserialize ESTree JSON: {e}")]),
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
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(filename).unwrap_or_default();
    let path = Path::new(filename);

    let ret = Parser::new(&allocator, source, source_type)
        .with_options(parser_options())
        .parse();

    if !ret.errors.is_empty() {
        return error_to_term(env, &format_errors(&ret.errors));
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
        return error_to_term(env, &format_errors(&result.errors));
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
