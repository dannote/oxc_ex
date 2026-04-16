use rayon::prelude::*;
use rustler::{Encoder, Env, NifResult, Term};

use crate::options::{decode_options, TransformInput};
use crate::parse::{transform_source, TransformOutput};

#[rustler::nif(schedule = "DirtyCpu")]
pub fn transform_many<'a>(
    env: Env<'a>,
    inputs: Vec<(String, String)>,
    opts_term: Term<'a>,
) -> NifResult<Term<'a>> {
    let opts = decode_options::<TransformInput>(opts_term);

    let outputs: Vec<TransformOutput> = inputs
        .par_iter()
        .map(|(source, filename)| transform_source(source, filename, &opts))
        .collect();

    let terms: Vec<Term<'a>> = outputs
        .into_iter()
        .map(|output| output.to_term(env))
        .collect();

    Ok(terms.encode(env))
}
