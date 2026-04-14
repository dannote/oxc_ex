use std::collections::BTreeMap;

use rustler::serde::from_term;
use rustler::Term;
use serde::de::DeserializeOwned;
use serde::Deserialize;

// Routes through serde_json::Value because Rustler's serde deserializer
// cannot handle string-keyed Elixir maps directly (atoms work, but the
// Elixir side sends string keys for serde #[serde(rename)] compatibility).
pub fn decode_options<T: DeserializeOwned + Default>(term: Term<'_>) -> T {
    from_term::<serde_json::Value>(term)
        .ok()
        .and_then(|value| serde_json::from_value::<T>(value).ok())
        .unwrap_or_default()
}

pub fn default_true() -> bool {
    true
}

pub fn default_jsx_runtime() -> String {
    "automatic".to_string()
}

pub fn default_format() -> String {
    "iife".to_string()
}

#[derive(Deserialize)]
#[serde(default)]
pub struct TransformInput {
    #[serde(rename = "jsx", default = "default_jsx_runtime")]
    pub jsx_runtime: String,
    pub jsx_factory: String,
    pub jsx_fragment: String,
    pub import_source: String,
    pub target: String,
    pub sourcemap: bool,
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
pub struct MinifyInput {
    #[serde(default = "default_true")]
    pub mangle: bool,
}

impl Default for MinifyInput {
    fn default() -> Self {
        Self { mangle: true }
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct BundleOptions {
    pub entry: String,
    #[serde(default = "default_format")]
    pub format: String,
    pub minify: bool,
    pub treeshake: bool,
    pub banner: Option<String>,
    pub footer: Option<String>,
    pub preamble: Option<String>,
    pub define: BTreeMap<String, String>,
    pub sourcemap: bool,
    pub drop_console: bool,
    #[serde(rename = "jsx", default = "default_jsx_runtime")]
    pub jsx_runtime: String,
    pub jsx_factory: String,
    pub jsx_fragment: String,
    pub import_source: String,
    pub target: String,
}
