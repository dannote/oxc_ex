defmodule OXC.BundleTest do
  use ExUnit.Case, async: true

  describe "bundle/2 basic behavior" do
    test "bundles a single file into valid JavaScript" do
      files = [{"a.ts", "const x: number = 1; (globalThis as any).x = x;"}]
      {:ok, js} = OXC.bundle(files, entry: "a.ts")

      assert_valid_bundle(js)
      assert js =~ "globalThis.x = x"
      refute js =~ "number"
      refute js =~ "import "
      refute js =~ "export "
    end

    test "strips TypeScript and resolves local imports" do
      files = [
        {"a.ts", "export const x: number = 1;"},
        {"b.ts", "import { x } from './a'\n(globalThis as any).val = x;"}
      ]

      {:ok, js} = OXC.bundle(files, entry: "b.ts")
      assert_valid_bundle(js)
      assert js =~ "globalThis.val = x"
      refute js =~ "number"
      refute js =~ "import "
      refute js =~ "export "
    end

    test "orders dependencies before dependents" do
      files = [
        {"b.ts", "import { A } from './a'\nexport class B extends A {}"},
        {"a.ts", "export class A {}"}
      ]

      {:ok, js} = OXC.bundle(files, entry: "b.ts")
      assert_valid_bundle(js)
      assert position_of(js, ["class A", "A = class"]) < position_of(js, ["class B", "B = class"])
    end

    test "handles diamond dependency graphs" do
      files = [
        {"entry.ts", "import { d } from './d'\nconsole.log(d);"},
        {"d.ts",
         "import { B } from './b'\nimport { C } from './c'\nexport const d = [B, C].length;"},
        {"b.ts", "import { A } from './a'\nexport class B extends A {}"},
        {"c.ts", "import { A } from './a'\nexport class C extends A {}"},
        {"a.ts", "export class A {}"}
      ]

      {:ok, js} = OXC.bundle(files, entry: "entry.ts")
      assert_valid_bundle(js)

      a_pos = position_of(js, ["class A", "A = class"])
      b_pos = position_of(js, ["class B", "B = class"])
      c_pos = position_of(js, ["class C", "C = class"])

      assert a_pos < b_pos
      assert a_pos < c_pos
    end

    test "ignores type-only imports" do
      files = [
        {"entry.ts", "import { A } from './a'\nimport { B } from './b'\nconsole.log(A, B);"},
        {"a.ts", "import type { B } from './b'\nexport class A { b?: any }"},
        {"b.ts", "import type { A } from './a'\nexport class B { a?: any }"}
      ]

      {:ok, js} = OXC.bundle(files, entry: "entry.ts")
      assert_valid_bundle(js)
      assert js =~ "A"
      assert js =~ "B"
    end

    test "handles named, default, aliased, and namespace imports" do
      files = [
        {"dep.ts",
         "export default 42; export function greet() { return 'hi' } export const value = 1;"},
        {"entry.ts",
         "import answer, { greet as hello } from './dep'; import * as ns from './dep'; console.log(answer, hello(), ns.value);"}
      ]

      {:ok, js} = OXC.bundle(files, entry: "entry.ts")
      assert_valid_bundle(js)
      assert js =~ "greet"
      assert js =~ "value"
      assert js =~ "42"
      refute js =~ "import "
      refute js =~ "export "
    end

    test "supports export forms" do
      files = [
        {"a.ts",
         "export class Foo {}\nexport const BAR = 42;\nexport default function greet() { return 'hi' }"}
      ]

      {:ok, js} = OXC.bundle(files, entry: "a.ts")
      assert_valid_bundle(js)
      assert js =~ "Foo"
      assert js =~ "BAR"
      assert js =~ "greet"
      refute js =~ "export "
    end

    test "supports anonymous default exports" do
      files = [
        {"widget.ts", "export default function() { return 'ok' }"},
        {"entry.ts", "import render from './widget'; console.log(render());"}
      ]

      {:ok, js} = OXC.bundle(files, entry: "entry.ts")
      assert_valid_bundle(js)
      assert js =~ "ok"
    end

    test "handles side-effect-only imports" do
      files = [
        {"setup.ts", "(globalThis as any).ready = true;"},
        {"main.ts", "import './setup'"}
      ]

      {:ok, js} = OXC.bundle(files, entry: "main.ts")
      assert_valid_bundle(js)
      assert js =~ "globalThis.ready = true"
    end

    test "resolves imports with .js specifiers to TypeScript files" do
      files = [
        {"a.ts", "export const x = 1;"},
        {"b.ts", "import { x } from './a.js'\n(globalThis as any).val = x;"}
      ]

      {:ok, js} = OXC.bundle(files, entry: "b.ts")
      assert_valid_bundle(js)
      assert js =~ "globalThis.val = x"
    end

    test "resolves nested paths without basename collisions" do
      files = [
        {"src/index.ts", "export const src = 1;"},
        {"lib/index.ts", "export const lib = 2;"},
        {"entry.ts",
         "import { src } from './src/index'; import { lib } from './lib/index'; console.log(JSON.stringify([src, lib]));"}
      ]

      {:ok, js} = OXC.bundle(files, entry: "entry.ts")
      assert_valid_bundle(js)
      assert js =~ "src"
      assert js =~ "lib"
    end

    test "keeps modules isolated enough for duplicate local bindings" do
      files = [
        {"comp_a.js",
         ~S[const _hoisted_1 = { class: "text-red" }; export function renderA() { return _hoisted_1; }]},
        {"comp_b.js",
         ~S[const _hoisted_1 = { class: "text-blue" }; export function renderB() { return _hoisted_1; }]},
        {"entry.js",
         ~S|import { renderA } from "./comp_a.js"; import { renderB } from "./comp_b.js"; console.log(JSON.stringify([renderA(), renderB()]));|}
      ]

      {:ok, js} = OXC.bundle(files, entry: "entry.js")
      assert_valid_bundle(js)
      assert js =~ "text-red"
      assert js =~ "text-blue"
    end

    test "returns errors for invalid syntax" do
      files = [{"bad.ts", "const = ;"}]
      assert {:error, [_ | _]} = OXC.bundle(files, entry: "bad.ts")
    end

    test "requires at least one file" do
      assert {:error, [message]} = OXC.bundle([], entry: "main.ts")
      assert message =~ "at least one file"
    end

    test "requires entry" do
      files = [{"a.ts", "export const x = 1;"}]
      assert {:error, [message]} = OXC.bundle(files)
      assert message =~ ":entry"
    end

    test "requires entry to exist in files" do
      files = [{"a.ts", "export const x = 1;"}]
      assert {:error, [message]} = OXC.bundle(files, entry: "missing.ts")
      assert message =~ "was not found"
    end

    test "handles circular dependencies without crashing" do
      files = [
        {"a.ts", "import { B } from './b'\nexport class A extends B {}"},
        {"b.ts", "import { A } from './a'\nexport class B extends A {}"}
      ]

      {:ok, js} = OXC.bundle(files, entry: "a.ts")
      assert_valid_bundle(js)
      assert js =~ "A"
      assert js =~ "B"
    end
  end

  describe "bundle/2 options" do
    test "minifies output" do
      files = [{"a.ts", "const longName: number = 42; (globalThis as any).v = longName;"}]

      {:ok, normal} = OXC.bundle(files, entry: "a.ts")
      {:ok, minified} = OXC.bundle(files, entry: "a.ts", minify: true)

      assert byte_size(minified) < byte_size(normal)
    end

    test "minify folds constants" do
      files = [{"a.ts", "const x = 1 + 2; (globalThis as any).x = x;"}]
      {:ok, js} = OXC.bundle(files, entry: "a.ts", minify: true)
      assert js =~ "3"
    end

    test "drop_console removes console calls when minifying" do
      files = [
        {"a.ts", "console.log('hi'); console.warn('careful'); (globalThis as any).x = 1;"}
      ]

      {:ok, js} = OXC.bundle(files, entry: "a.ts", minify: true, drop_console: true)
      refute js =~ "console"
      assert js =~ "1"
    end

    test "banner and footer are preserved" do
      files = [{"a.ts", "const x = 1;"}]
      {:ok, js} = OXC.bundle(files, entry: "a.ts", banner: "/* top */", footer: "/* bottom */")

      assert String.starts_with?(js, "/* top */")
      assert String.ends_with?(String.trim(js), "/* bottom */")
    end

    test "preamble injects code at the top of IIFE body" do
      files = [{"a.ts", "const x = 1; (globalThis as any).x = x;"}]

      {:ok, js} =
        OXC.bundle(files, entry: "a.ts", preamble: "const { ref } = Vue;")

      assert_valid_bundle(js)
      assert js =~ "const { ref } = Vue;"
      iife_start = js |> String.split("const { ref } = Vue;") |> hd()
      assert iife_start =~ "function"
    end

    test "preamble works with minification" do
      files = [{"a.ts", "const x = 1; (globalThis as any).x = x;"}]

      {:ok, js} =
        OXC.bundle(files, entry: "a.ts", preamble: "const { ref } = Vue;", minify: true)

      assert js =~ "Vue"
    end

    test "define replaces identifiers" do
      files = [{"a.ts", "const env = process.env.NODE_ENV; (globalThis as any).env = env;"}]

      {:ok, js} =
        OXC.bundle(files, entry: "a.ts", define: %{"process.env.NODE_ENV" => ~s("production")})

      assert js =~ ~s("production")
      refute js =~ "process.env"
    end

    test "define combines with minify for dead code elimination" do
      files = [
        {"a.ts",
         "if (process.env.NODE_ENV === 'development') { console.log('dev') } (globalThis as any).x = 1;"}
      ]

      {:ok, js} =
        OXC.bundle(files,
          entry: "a.ts",
          define: %{"process.env.NODE_ENV" => ~s("production")},
          minify: true
        )

      refute js =~ "dev"
    end

    test "jsx classic pragma is supported" do
      files = [{"app.jsx", "export const App = () => <div>hello</div>"}]
      {:ok, js} = OXC.bundle(files, entry: "app.jsx", jsx: :classic, jsx_factory: "h")

      assert js =~ "h("
      refute js =~ "createElement"
    end

    test "jsx fragment configuration is supported" do
      files = [{"app.jsx", "export const App = () => <><span /></>"}]

      {:ok, js} =
        OXC.bundle(files,
          entry: "app.jsx",
          jsx: :classic,
          jsx_factory: "h",
          jsx_fragment: "Fragment"
        )

      assert js =~ "Fragment"
      refute js =~ "React"
    end

    test "automatic jsx runtime remains the default" do
      files = [{"app.jsx", "export const App = () => <div />"}]
      {:ok, js} = OXC.bundle(files, entry: "app.jsx")

      assert js =~ "jsx"
      refute js =~ "createElement"
    end

    test "target downlevels syntax" do
      files = [{"a.js", "const x = a ?? b; (globalThis).x = x;"}]
      {:ok, js} = OXC.bundle(files, entry: "a.js", target: "es2019")
      refute js =~ "??"
    end

    test "format :esm produces ES module output" do
      files = [
        {"util.ts", "export const x: number = 1;"},
        {"main.ts", "import { x } from './util'\nexport const y = x + 1;"}
      ]

      {:ok, js} = OXC.bundle(files, entry: "main.ts", format: :esm)
      assert js =~ "export"
      refute js =~ "(function"
    end

    test "format :cjs produces CommonJS output" do
      files = [{"main.ts", "export const x: number = 1;"}]
      {:ok, js} = OXC.bundle(files, entry: "main.ts", format: :cjs)
      assert js =~ "exports"
      refute js =~ "(function"
    end

    test "format defaults to :iife" do
      files = [{"main.ts", "const x = 1;"}]
      {:ok, js} = OXC.bundle(files, entry: "main.ts")
      assert js =~ "(function"
    end
  end

  describe "bundle/2 sourcemaps" do
    test "returns code and sourcemap" do
      files = [{"a.ts", "const x: number = 1; (globalThis as any).x = x;"}]
      {:ok, result} = OXC.bundle(files, entry: "a.ts", sourcemap: true)

      assert is_map(result)
      assert is_binary(result.code)
      assert is_binary(result.sourcemap)
    end

    test "sourcemap points to original sources" do
      files = [
        {"a.ts", "export const x = 1;"},
        {"b.ts", "import { x } from './a'; console.log(x);"}
      ]

      {:ok, result} = OXC.bundle(files, entry: "b.ts", sourcemap: true)
      assert {:ok, map} = Jason.decode(result.sourcemap)
      assert map["version"] == 3
      assert_sources_include(map["sources"], ["a.ts", "b.ts"])
      refute Enum.any?(map["sources"], &String.ends_with?(&1, "bundle.js"))
    end

    test "sourcemap works with minify" do
      files = [
        {"a.ts", "export const x = 1;"},
        {"b.ts", "import { x } from './a'; console.log(x);"}
      ]

      {:ok, result} = OXC.bundle(files, entry: "b.ts", minify: true, sourcemap: true)
      assert {:ok, map} = Jason.decode(result.sourcemap)
      assert is_binary(result.code)
      assert map["version"] == 3
      assert_sources_include(map["sources"], ["a.ts", "b.ts"])
    end
  end

  describe "bundle!/2" do
    test "returns bundled output on success" do
      files = [{"a.ts", "const x: number = 1;"}]
      js = OXC.bundle!(files, entry: "a.ts")

      assert is_binary(js)
      assert_valid_bundle(js)
      refute js =~ "number"
    end

    test "returns code and sourcemap when requested" do
      files = [{"a.ts", "const x = 1;"}]
      result = OXC.bundle!(files, entry: "a.ts", sourcemap: true)

      assert is_map(result)
      assert is_binary(result.code)
      assert is_binary(result.sourcemap)
    end

    test "raises on errors" do
      assert_raise RuntimeError, ~r/bundle error/, fn ->
        OXC.bundle!([{"bad.ts", "const = ;"}], entry: "bad.ts")
      end
    end
  end

  defp assert_valid_bundle(js) do
    assert OXC.valid?(js, "bundle.js")
    assert {:ok, _ast} = OXC.parse(js, "bundle.js")
  end

  defp assert_sources_include(sources, expected_files) do
    Enum.each(expected_files, fn file ->
      assert Enum.any?(sources, &String.ends_with?(&1, file))
    end)
  end

  defp position_of(js, needles) do
    needles
    |> Enum.find_value(fn needle ->
      case :binary.match(js, needle) do
        {index, _length} -> index
        :nomatch -> nil
      end
    end)
    |> case do
      nil -> flunk("could not find any of #{inspect(needles)} in bundle")
      index -> index
    end
  end
end
