defmodule OXC do
  @moduledoc """
  Elixir bindings for the [OXC](https://oxc.rs) JavaScript toolchain.

  Provides fast JavaScript and TypeScript parsing, transformation, and
  minification via Rust NIFs. The file extension determines the dialect —
  `.js`, `.jsx`, `.ts`, `.tsx`.

      iex> {:ok, ast} = OXC.parse("const x = 1 + 2", "test.js")
      iex> ast.type
      :program

      iex> {:ok, js} = OXC.transform("const x: number = 42", "test.ts")
      iex> js
      "const x = 42;\\n"

  AST nodes are maps with atom keys, following the ESTree specification.
  The `:type` and `:kind` field values are snake_case atoms
  (e.g. `:import_declaration`, `:variable_declaration`).
  """

  defmodule Error do
    defexception [:message, :errors]

    @impl true
    def message(%{message: message}), do: message
  end

  @type ast :: %{required(:type) => atom(), optional(atom()) => any()}
  @type error :: %{message: String.t()}
  @type code_with_sourcemap :: %{code: String.t(), sourcemap: String.t()}
  @type parse_result :: {:ok, ast()} | {:error, [error()]}
  @type transform_result :: {:ok, String.t() | code_with_sourcemap()} | {:error, [error()]}
  @type bundle_result :: {:ok, String.t() | code_with_sourcemap()} | {:error, [error()]}

  @doc """
  Parse JavaScript or TypeScript source code into an ESTree AST.

  The filename extension determines the dialect:
  - `.js` — JavaScript
  - `.jsx` — JavaScript with JSX
  - `.ts` — TypeScript
  - `.tsx` — TypeScript with JSX

  Returns `{:ok, ast}` where `ast` is a map with atom keys, or
  `{:error, errors}` with a list of parse error maps.

  ## Examples

      iex> {:ok, ast} = OXC.parse("const x = 1", "test.js")
      iex> [decl] = ast.body
      iex> decl.type
      :variable_declaration

      iex> {:error, [%{message: msg} | _]} = OXC.parse("const = ;", "bad.js")
      iex> is_binary(msg)
      true
  """
  @spec parse(String.t(), String.t()) :: parse_result()
  def parse(source, filename) do
    case OXC.Native.parse(source, filename) do
      {:ok, ast} -> {:ok, atomize_term_keys(ast)}
      {:error, errors} -> {:error, atomize_term_keys(errors)}
    end
  end

  @doc """
  Like `parse/2` but raises on parse errors.

  ## Examples

      iex> ast = OXC.parse!("const x = 1", "test.js")
      iex> ast.type
      :program
  """
  @spec parse!(String.t(), String.t()) :: ast()
  def parse!(source, filename) do
    case parse(source, filename) do
      {:ok, ast} ->
        ast

      {:error, errors} ->
        raise Error, message: "OXC parse error: #{inspect(errors)}", errors: errors
    end
  end

  @doc """
  Check if source code is syntactically valid.

  Faster than `parse/2` — skips AST serialization.

  ## Examples

      iex> OXC.valid?("const x = 1", "test.js")
      true

      iex> OXC.valid?("const = ;", "bad.js")
      false
  """
  @spec valid?(String.t(), String.t()) :: boolean()
  def valid?(source, filename) do
    OXC.Native.valid(source, filename)
  end

  @doc """
  Transform TypeScript/JSX source code into plain JavaScript.

  Strips type annotations, transforms JSX, and lowers syntax features.
  The filename extension determines the source dialect.

  ## Options

    * `:jsx` — JSX runtime, `:automatic` (default) or `:classic`
    * `:jsx_factory` — function for classic JSX (default: `"React.createElement"`)
    * `:jsx_fragment` — fragment for classic JSX (default: `"React.Fragment"`)
    * `:import_source` — JSX import source (e.g. `"vue"`, `"preact"`)
    * `:target` — downlevel target (e.g. `"es2019"`, `"chrome80"`)
    * `:sourcemap` — generate a source map (default: `false`). When `true`,
      returns `%{code: String.t(), sourcemap: String.t()}` instead of a plain string.

  ## Examples

      iex> {:ok, js} = OXC.transform("const x: number = 42", "test.ts")
      iex> js
      "const x = 42;\\n"

      iex> {:ok, js} = OXC.transform("<div />", "c.jsx", jsx: :classic)
      iex> js =~ "createElement"
      true
  """
  @spec transform(String.t(), String.t(), keyword()) :: transform_result()
  def transform(source, filename, opts \\ []) do
    case OXC.Native.transform(source, filename, normalize_transform_options(opts)) do
      {:ok, result} -> {:ok, normalize_native_result(result)}
      {:error, errors} -> {:error, atomize_term_keys(errors)}
    end
  end

  @doc """
  Like `transform/3` but raises on errors.

  ## Examples

      iex> OXC.transform!("const x: number = 42", "test.ts")
      "const x = 42;\\n"
  """
  @spec transform!(String.t(), String.t(), keyword()) :: String.t() | code_with_sourcemap()
  def transform!(source, filename, opts \\ []) do
    case transform(source, filename, opts) do
      {:ok, code} ->
        code

      {:error, errors} ->
        raise Error, message: "OXC transform error: #{inspect(errors)}", errors: errors
    end
  end

  @doc """
  Transform multiple source files in parallel using a Rust thread pool.

  Accepts a list of `{source, filename}` tuples and shared options.
  Returns a list of results in the same order, each being
  `{:ok, code}`, `{:ok, %{code: ..., sourcemap: ...}}`, or `{:error, errors}`.

  Significantly faster than calling `transform/3` sequentially for many files,
  since work is distributed across OS threads without BEAM scheduling overhead.

  ## Examples

      iex> results = OXC.transform_many([{"const x: number = 1", "a.ts"}, {"const y: number = 2", "b.ts"}])
      iex> length(results)
      2
      iex> {:ok, code} = hd(results)
      iex> code =~ "const x = 1"
      true
  """
  @spec transform_many([{String.t(), String.t()}], keyword()) :: [transform_result()]
  def transform_many(inputs, opts \\ []) do
    native_opts = normalize_transform_options(opts)

    OXC.Native.transform_many(inputs, native_opts)
    |> Enum.map(fn
      {:ok, result} -> {:ok, normalize_native_result(result)}
      {:error, errors} -> {:error, atomize_term_keys(errors)}
    end)
  end

  @doc """
  Minify JavaScript source code.

  Applies dead code elimination, constant folding, and whitespace removal.
  Optionally mangles variable names for smaller output.

  ## Options

    * `:mangle` — rename variables for shorter names (default: `true`)

  ## Examples

      iex> {:ok, min} = OXC.minify("if (false) { x() } y();", "test.js")
      iex> min =~ "y()"
      true
      iex> min =~ "x()"
      false
  """
  @spec minify(String.t(), String.t(), keyword()) :: {:ok, String.t()} | {:error, [error()]}
  def minify(source, filename, opts \\ []) do
    case OXC.Native.minify(source, filename, normalize_minify_options(opts)) do
      {:ok, code} -> {:ok, code}
      {:error, errors} -> {:error, atomize_term_keys(errors)}
    end
  end

  @doc """
  Like `minify/3` but raises on errors.

  ## Examples

      iex> min = OXC.minify!("const x = 1 + 2;", "test.js")
      iex> is_binary(min)
      true
  """
  @spec minify!(String.t(), String.t(), keyword()) :: String.t()
  def minify!(source, filename, opts \\ []) do
    case minify(source, filename, opts) do
      {:ok, code} ->
        code

      {:error, errors} ->
        raise Error, message: "OXC minify error: #{inspect(errors)}", errors: errors
    end
  end

  @doc """
  Extract import specifiers from JavaScript/TypeScript source.

  Faster than `parse/2` + `collect/2` — skips full AST serialization
  and returns only the import source strings. Type-only imports
  (`import type { ... }`) are excluded.

  ## Examples

      iex> {:ok, imports} = OXC.imports("import { ref } from 'vue'\\nimport type { Ref } from 'vue'", "test.ts")
      iex> imports
      ["vue"]
  """
  @spec imports(String.t(), String.t()) :: {:ok, [String.t()]} | {:error, [error()]}
  def imports(source, filename) do
    case OXC.Native.imports(source, filename) do
      {:ok, list} -> {:ok, list}
      {:error, errors} -> {:error, atomize_term_keys(errors)}
    end
  end

  @doc "Like `imports/2` but raises on errors."
  @spec imports!(String.t(), String.t()) :: [String.t()]
  def imports!(source, filename) do
    case imports(source, filename) do
      {:ok, list} ->
        list

      {:error, errors} ->
        raise Error, message: "OXC imports error: #{inspect(errors)}", errors: errors
    end
  end

  @doc """
  Analyze imports with type information.

  Returns `{:ok, list}` where each element is a map with:
    * `:specifier` — the import source string (e.g. `"vue"`, `"./utils"`)
    * `:type` — `:static` or `:dynamic`
    * `:kind` — `:import`, `:export`, or `:export_all`
    * `:start` — byte offset of the specifier string literal (including quote)
    * `:end` — byte offset of the end of the specifier string literal

  Type-only imports/exports (`import type { ... }`, `export type { ... }`)
  are excluded.

  ## Examples

      iex> source = "import { ref } from 'vue'\\nexport { foo } from './bar'\\nimport('./lazy')"
      iex> {:ok, imports} = OXC.collect_imports(source, "test.js")
      iex> Enum.map(imports, & &1.specifier)
      ["vue", "./bar", "./lazy"]
      iex> Enum.map(imports, & &1.type)
      [:static, :static, :dynamic]
      iex> Enum.map(imports, & &1.kind)
      [:import, :export, :import]
  """
  @spec collect_imports(String.t(), String.t()) ::
          {:ok,
           [
             %{
               specifier: String.t(),
               type: :static | :dynamic,
               kind: :import | :export | :export_all,
               start: non_neg_integer(),
               end: non_neg_integer()
             }
           ]}
          | {:error, [error()]}
  def collect_imports(source, filename) do
    case OXC.Native.collect_imports(source, filename) do
      {:ok, list} -> {:ok, list}
      {:error, errors} -> {:error, atomize_term_keys(errors)}
    end
  end

  @doc "Like `collect_imports/2` but raises on errors."
  @spec collect_imports!(String.t(), String.t()) :: [map()]
  def collect_imports!(source, filename) do
    case collect_imports(source, filename) do
      {:ok, list} ->
        list

      {:error, errors} ->
        raise Error, message: "OXC collect_imports error: #{inspect(errors)}", errors: errors
    end
  end

  @doc """
  Rewrite import/export specifiers in a single pass.

  Parses the source, finds all import/export declarations
  (ImportDeclaration, ExportNamedDeclaration, ExportAllDeclaration,
  and dynamic ImportExpression), and calls `fun` with each specifier string.

  The callback returns:
    * `{:rewrite, new_specifier}` — replace the specifier
    * `:keep` — leave unchanged

  Returns `{:ok, patched_source}` or `{:error, errors}`.

  ## Examples

      iex> source = "import { ref } from 'vue'\\nimport a from './utils'"
      iex> {:ok, result} = OXC.rewrite_specifiers(source, "test.js", fn
      ...>   "vue" -> {:rewrite, "/@vendor/vue.js"}
      ...>   _ -> :keep
      ...> end)
      iex> result
      "import { ref } from '/@vendor/vue.js'\\nimport a from './utils'"
  """
  @spec rewrite_specifiers(String.t(), String.t(), (String.t() -> {:rewrite, String.t()} | :keep)) ::
          {:ok, String.t()} | {:error, [error()]}
  def rewrite_specifiers(source, filename, fun) when is_function(fun, 1) do
    case collect_imports(source, filename) do
      {:ok, imports} ->
        patches =
          Enum.reduce(imports, [], fn %{specifier: spec, start: s, end: e}, acc ->
            case fun.(spec) do
              {:rewrite, new} -> [%{start: s + 1, end: e - 1, change: new} | acc]
              :keep -> acc
            end
          end)

        {:ok, patch_string(source, patches)}

      {:error, _} = error ->
        error
    end
  end

  @doc """
  Like `rewrite_specifiers/3` but raises on errors.
  """
  @spec rewrite_specifiers!(String.t(), String.t(), (String.t() -> {:rewrite, String.t()} | :keep)) ::
          String.t()
  def rewrite_specifiers!(source, filename, fun) do
    case rewrite_specifiers(source, filename, fun) do
      {:ok, result} ->
        result

      {:error, errors} ->
        raise Error, message: "OXC rewrite_specifiers error: #{inspect(errors)}", errors: errors
    end
  end

  @doc """
  Bundle multiple TypeScript/JavaScript modules into a single IIFE script.

  Takes a list of `{filename, source}` tuples representing a virtual project.
  Modules can import each other via relative paths and are bundled into a
  single IIFE script.

  ## Options

    * `:entry` — entry module filename from `files` (required), for example
      `"main.ts"`
    * `:format` — output format: `:iife` (default), `:esm`, or `:cjs`
    * `:minify` — minify the output (default: `false`)
    * `:treeshake` — enable tree-shaking to remove unused exports (default: `false`)
    * `:banner` — string to prepend before the IIFE (e.g. `"/* v1.0 */"`)
    * `:footer` — string to append after the IIFE
    * `:preamble` — code to inject at the top of the IIFE function body,
      before any bundled modules (e.g. `"const { ref } = Vue;"`)
    * `:define` — compile-time replacements, map of `%{"process.env.NODE_ENV" => ~s("production")}`
    * `:sourcemap` — generate a source map (default: `false`). When `true`,
      returns `%{code: String.t(), sourcemap: String.t()}` instead of a plain string.
    * `:drop_console` — remove `console.*` calls during minification (default: `false`)
    * `:jsx` — JSX runtime, `:automatic` (default) or `:classic`
    * `:jsx_factory` — function for classic JSX (default: `"React.createElement"`)
    * `:jsx_fragment` — fragment for classic JSX (default: `"React.Fragment"`)
    * `:import_source` — JSX import source (e.g. `"vue"`, `"preact"`)
    * `:target` — downlevel target (e.g. `"es2019"`, `"chrome80"`)

  ## Examples

      iex> files = [
      ...>   {"event.ts", "export class Event { type: string; constructor(type: string) { this.type = type } }"},
      ...>   {"target.ts", "import { Event } from './event'\\nexport class Target extends Event {}"}
      ...> ]
      iex> {:ok, js} = OXC.bundle(files, entry: "target.ts")
      iex> String.contains?(js, "Event")
      true
      iex> String.contains?(js, "Target")
      true
      iex> String.contains?(js, "import ")
      false
  """
  @spec bundle([{String.t(), String.t()}], keyword()) :: bundle_result()
  def bundle(files, opts \\ []) do
    if Keyword.get(opts, :entry, "") == "" do
      {:error, [%{message: "bundle/2 requires :entry, for example: entry: \"main.ts\""}]}
    else
      case OXC.Native.bundle(files, normalize_bundle_options(opts)) do
        {:ok, result} -> {:ok, normalize_native_result(result)}
        {:error, errors} -> {:error, atomize_term_keys(errors)}
      end
    end
  end

  @doc """
  Like `bundle/2` but raises on errors.
  """
  @spec bundle!([{String.t(), String.t()}], keyword()) :: String.t() | code_with_sourcemap()
  def bundle!(files, opts \\ []) do
    case bundle(files, opts) do
      {:ok, result} ->
        result

      {:error, errors} ->
        raise Error, message: "OXC bundle error: #{inspect(errors)}", errors: errors
    end
  end

  defp normalize_transform_options(opts) do
    %{
      "jsx" => normalize_jsx_runtime(Keyword.get(opts, :jsx, :automatic)),
      "jsx_factory" => Keyword.get(opts, :jsx_factory, ""),
      "jsx_fragment" => Keyword.get(opts, :jsx_fragment, ""),
      "import_source" => Keyword.get(opts, :import_source, ""),
      "target" => Keyword.get(opts, :target, ""),
      "sourcemap" => Keyword.get(opts, :sourcemap, false)
    }
  end

  defp normalize_minify_options(opts) do
    %{"mangle" => Keyword.get(opts, :mangle, true)}
  end

  defp normalize_bundle_options(opts) do
    %{
      "entry" => Keyword.get(opts, :entry, ""),
      "format" => opts |> Keyword.get(:format, :iife) |> Atom.to_string(),
      "minify" => Keyword.get(opts, :minify, false),
      "treeshake" => Keyword.get(opts, :treeshake, false),
      "banner" => Keyword.get(opts, :banner),
      "footer" => Keyword.get(opts, :footer),
      "preamble" => Keyword.get(opts, :preamble),
      "define" => Keyword.get(opts, :define, %{}),
      "sourcemap" => Keyword.get(opts, :sourcemap, false),
      "drop_console" => Keyword.get(opts, :drop_console, false),
      "jsx" => normalize_jsx_runtime(Keyword.get(opts, :jsx, :automatic)),
      "jsx_factory" => Keyword.get(opts, :jsx_factory, ""),
      "jsx_fragment" => Keyword.get(opts, :jsx_fragment, ""),
      "import_source" => Keyword.get(opts, :import_source, ""),
      "target" => Keyword.get(opts, :target, "")
    }
  end

  defp normalize_jsx_runtime(runtime) when is_atom(runtime), do: Atom.to_string(runtime)
  defp normalize_jsx_runtime(runtime) when is_binary(runtime), do: runtime
  defp normalize_jsx_runtime(_runtime), do: "automatic"

  defp normalize_native_result(result) when is_map(result), do: atomize_term_keys(result)
  defp normalize_native_result(result), do: result

  # Safe to use String.to_atom/1 here: ESTree has a fixed, bounded set of
  # property names and node types. Untrusted user input (JS source code)
  # only affects string *values*, not map keys — those come from OXC's
  # serializer which emits a known set of ESTree field names.
  defp atomize_term_keys(map) when is_map(map) do
    Map.new(map, fn {key, value} ->
      atom_key = if is_binary(key), do: String.to_atom(key), else: key
      {atom_key, atomize_value(atom_key, value)}
    end)
  end

  defp atomize_term_keys(list) when is_list(list), do: Enum.map(list, &atomize_term_keys/1)
  defp atomize_term_keys(value), do: value

  defp atomize_value(:type, value) when is_binary(value), do: to_snake_atom(value)
  defp atomize_value(:kind, value) when is_binary(value), do: to_snake_atom(value)
  defp atomize_value(_key, value), do: atomize_term_keys(value)

  defp to_snake_atom(value) do
    value |> Macro.underscore() |> String.to_atom()
  end

  # ── AST Traversal ──

  @doc """
  Walk an AST tree, calling `fun` on every node (any map with a `:type` key).

  Descends into all map values and list elements to reach nested AST
  nodes, including maps without a `:type` key (which are skipped for
  the callback but still traversed).

  ## Examples

      iex> {:ok, ast} = OXC.parse("const x = 1", "test.js")
      iex> OXC.walk(ast, fn
      ...>   %{type: :identifier, name: name} -> send(self(), {:id, name})
      ...>   _ -> :ok
      ...> end)
      iex> receive do {:id, name} -> name end
      "x"
  """
  @spec walk(ast() | [ast()], (map() -> any())) :: :ok
  def walk(nodes, fun) when is_list(nodes) and is_function(fun, 1) do
    Enum.each(nodes, &walk(&1, fun))
  end

  def walk(node, fun) when is_map(node) and is_function(fun, 1) do
    if Map.has_key?(node, :type), do: fun.(node)

    node
    |> Map.values()
    |> Enum.each(fn
      child when is_map(child) -> walk(child, fun)
      children when is_list(children) -> Enum.each(children, &walk_child(&1, fun))
      _ -> :ok
    end)
  end

  def walk(_node, _fun), do: :ok

  defp walk_child(node, fun) when is_map(node), do: walk(node, fun)
  defp walk_child(_node, _fun), do: :ok

  @doc """
  Depth-first post-order traversal, like `Macro.postwalk/2`.

  Visits every AST node (map with a `:type` key). Children are visited
  first, then the node itself. The callback returns the (possibly modified)
  node.

  Accepts a single AST node or a list of nodes.

  ## Examples

      iex> {:ok, ast} = OXC.parse("const x = 1", "test.js")
      iex> OXC.postwalk(ast, fn
      ...>   %{type: :identifier, name: "x"} = node -> %{node | name: "y"}
      ...>   node -> node
      ...> end)
      iex> :ok
      :ok
  """
  @spec postwalk(ast() | [ast()], (map() -> map())) :: map() | [map()]
  def postwalk(nodes, fun) when is_list(nodes) and is_function(fun, 1) do
    Enum.map(nodes, fn
      n when is_map(n) -> postwalk(n, fun)
      n -> n
    end)
  end

  def postwalk(node, fun) when is_map(node) and is_function(fun, 1) do
    updated =
      Map.new(node, fn
        {k, child} when is_map(child) ->
          {k, postwalk(child, fun)}

        {k, children} when is_list(children) ->
          {k,
           Enum.map(children, fn
             c when is_map(c) -> postwalk(c, fun)
             c -> c
           end)}

        pair ->
          pair
      end)

    if Map.has_key?(updated, :type), do: fun.(updated), else: updated
  end

  def postwalk(node, _fun), do: node

  @doc """
  Depth-first post-order traversal with accumulator, like `Macro.postwalk/3`.

  The callback receives each AST node and the accumulator, and must return
  `{node, acc}`. Use this to collect data while traversing.

  Accepts a single AST node or a list of nodes.

  ## Examples

      iex> source = "import { ref } from 'vue'\\nimport a from './utils'"
      iex> {:ok, ast} = OXC.parse(source, "test.ts")
      iex> {_ast, patches} = OXC.postwalk(ast, [], fn
      ...>   %{type: :import_declaration, source: %{value: "vue"} = src} = node, patches ->
      ...>     {node, [%{start: src.start, end: src.end, change: "'/@vendor/vue.js'"} | patches]}
      ...>   node, patches ->
      ...>     {node, patches}
      ...> end)
      iex> OXC.patch_string(source, patches)
      "import { ref } from '/@vendor/vue.js'\\nimport a from './utils'"
  """
  @spec postwalk(ast() | [ast()], acc, (map(), acc -> {map(), acc})) :: {map() | [map()], acc}
        when acc: term()
  def postwalk(nodes, acc, fun) when is_list(nodes) and is_function(fun, 2) do
    Enum.map_reduce(nodes, acc, fn
      node, a when is_map(node) -> postwalk(node, a, fun)
      node, a -> {node, a}
    end)
  end

  def postwalk(node, acc, fun) when is_map(node) and is_function(fun, 2) do
    {updated, acc} =
      Enum.reduce(Map.keys(node), {node, acc}, fn key, {n, a} ->
        case Map.fetch!(n, key) do
          child when is_map(child) ->
            {new_child, a} = postwalk(child, a, fun)
            {Map.put(n, key, new_child), a}

          children when is_list(children) ->
            {new_children, a} =
              Enum.map_reduce(children, a, fn
                child, a when is_map(child) -> postwalk(child, a, fun)
                child, a -> {child, a}
              end)

            {Map.put(n, key, new_children), a}

          _ ->
            {n, a}
        end
      end)

    if Map.has_key?(updated, :type), do: fun.(updated, acc), else: {updated, acc}
  end

  def postwalk(node, acc, _fun), do: {node, acc}

  @doc """
  Collect AST nodes that match a filter function.

  The function receives each node (map with `:type` key) and should return
  `{:keep, value}` to include it in results, or `:skip` to exclude it.

  ## Examples

      iex> {:ok, ast} = OXC.parse("const x = y + z", "test.js")
      iex> OXC.collect(ast, fn
      ...>   %{type: :identifier, name: name} -> {:keep, name}
      ...>   _ -> :skip
      ...> end)
      ["x", "y", "z"]
  """
  @spec collect(ast(), (map() -> {:keep, any()} | :skip)) :: [any()]
  def collect(node, fun) do
    node
    |> do_collect(fun, [])
    |> Enum.reverse()
  end

  defp do_collect(node, fun, acc) when is_map(node) do
    acc =
      if Map.has_key?(node, :type) do
        case fun.(node) do
          {:keep, value} -> [value | acc]
          :skip -> acc
        end
      else
        acc
      end

    Enum.reduce(Map.values(node), acc, fn
      child, a when is_map(child) ->
        do_collect(child, fun, a)

      children, a when is_list(children) ->
        Enum.reduce(children, a, &do_collect_child(&1, fun, &2))

      _, a ->
        a
    end)
  end

  defp do_collect(_node, _fun, acc), do: acc

  defp do_collect_child(node, fun, acc) when is_map(node), do: do_collect(node, fun, acc)
  defp do_collect_child(_node, _fun, acc), do: acc

  # ── Source Patching ──

  @type patch :: %{start: non_neg_integer(), end: non_neg_integer(), change: String.t()}

  @doc """
  Apply patches to source code, like `Sourceror.patch_string/2`.

  Each patch is a map with `:start` (byte offset), `:end` (byte offset),
  and `:change` (replacement string). Patches are applied in reverse
  offset order so that earlier patches don't shift later offsets.

  When multiple patches target the same `{start, end}` range, only the
  first one is applied and duplicates are silently dropped.

  Use with `postwalk/3` to collect patches from the AST, then apply
  them to the original source string.

  ## Examples

      iex> OXC.patch_string("hello world", [%{start: 6, end: 11, change: "elixir"}])
      "hello elixir"

      iex> source = "import { ref } from 'vue'"
      iex> OXC.patch_string(source, [%{start: 20, end: 25, change: "'/@vendor/vue.js'"}])
      "import { ref } from '/@vendor/vue.js'"
  """
  @spec patch_string(String.t(), [patch()]) :: String.t()
  def patch_string(source, patches) do
    patches
    |> Enum.uniq_by(fn %{start: s, end: e} -> {s, e} end)
    |> Enum.sort_by(fn %{start: s} -> s end, :desc)
    |> Enum.reduce(source, fn %{start: s, end: e, change: replacement}, acc ->
      binary_part(acc, 0, s) <> replacement <> binary_part(acc, e, byte_size(acc) - e)
    end)
  end
end
