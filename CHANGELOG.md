# Changelog

## 0.5.2

- Fix `default` keyword rejected as property name when minifying bundled IIFE output. The minifier was re-parsing in ESM module mode (`SourceType::mjs`) instead of script mode — `{ default: value }` and `obj.default` are valid JavaScript but were rejected as reserved words in module context.

## 0.5.1

- Handle circular dependencies in bundler's topological sort — modules in a cycle are appended in sorted order instead of raising an error. Enables bundling Vue, Reka UI, and other frameworks with circular imports.

## 0.5.0

- Initial precompiled NIF release (aarch64-apple-darwin, x86_64-apple-darwin, x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu, x86_64-unknown-linux-musl)
- Move to elixir-volt org

## 0.4.0

- `OXC.bundle/2` — bundle multiple JS/TS modules into a single IIFE with topological sorting and import resolution
- `OXC.imports/2` — extract import specifiers from source
- `OXC.postwalk/3` — AST traversal with accumulator for source patching
- `OXC.patch_string/2` — apply byte-offset patches
- Compile-time replacements via `:define` option
- Source map support in bundle and minify
