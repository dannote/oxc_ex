defmodule OXC.BundleTest do
  use ExUnit.Case, async: true

  describe "bundle/2" do
    test "bundles single file into IIFE" do
      files = [{"a.ts", "const x: number = 1; (globalThis as any).x = x;"}]
      {:ok, js} = OXC.bundle(files)
      assert js =~ "(() => {"
      assert js =~ "const x = 1"
      assert js =~ "})();"
      refute js =~ "number"
    end

    test "strips TypeScript from all files" do
      files = [
        {"a.ts", "export const x: number = 1;"},
        {"b.ts", "import { x } from './a'\n(globalThis as any).val = x;"}
      ]

      {:ok, js} = OXC.bundle(files)
      refute js =~ "number"
      refute js =~ "import"
      refute js =~ "export"
    end

    test "resolves dependency order" do
      files = [
        {"b.ts", "import { A } from './a'\nexport class B extends A {}"},
        {"a.ts", "export class A {}"}
      ]

      {:ok, js} = OXC.bundle(files)
      a_pos = :binary.match(js, "class A") |> elem(0)
      b_pos = :binary.match(js, "class B") |> elem(0)
      assert a_pos < b_pos
    end

    test "handles diamond dependency graph" do
      files = [
        {"d.ts", "import { B } from './b'\nimport { C } from './c'\n(globalThis as any).d = 1;"},
        {"b.ts", "import { A } from './a'\nexport class B extends A {}"},
        {"c.ts", "import { A } from './a'\nexport class C extends A {}"},
        {"a.ts", "export class A {}"}
      ]

      {:ok, js} = OXC.bundle(files)
      a_pos = :binary.match(js, "class A") |> elem(0)
      b_pos = :binary.match(js, "class B") |> elem(0)
      c_pos = :binary.match(js, "class C") |> elem(0)
      assert a_pos < b_pos
      assert a_pos < c_pos
    end

    test "ignores type-only imports for dependency ordering" do
      files = [
        {"a.ts", "import type { B } from './b'\nexport class A { b?: any }"},
        {"b.ts", "import type { A } from './a'\nexport class B { a?: any }"}
      ]

      {:ok, js} = OXC.bundle(files)
      assert js =~ "class A"
      assert js =~ "class B"
    end

    test "drops import declarations" do
      files = [
        {"a.ts", "export const x = 1;"},
        {"b.ts", "import { x } from './a'\n(globalThis as any).val = x;"}
      ]

      {:ok, js} = OXC.bundle(files)
      refute js =~ "import"
    end

    test "unwraps export named declarations" do
      files = [{"a.ts", "export class Foo {}\nexport const BAR = 42;"}]
      {:ok, js} = OXC.bundle(files)
      assert js =~ "class Foo"
      assert js =~ "const BAR = 42"
      refute js =~ "export"
    end

    test "unwraps export default function" do
      files = [{"a.ts", "export default function greet() { return 'hi' }"}]
      {:ok, js} = OXC.bundle(files)
      assert js =~ "function greet()"
      refute js =~ "export"
    end

    test "unwraps export default class" do
      files = [{"a.ts", "export default class Widget {}"}]
      {:ok, js} = OXC.bundle(files)
      assert js =~ "class Widget"
      refute js =~ "export"
    end

    test "supports renamed export specifiers" do
      files = [
        {"impl.ts", "function greetImpl() { return 'hi' }\nexport { greetImpl as greet }"},
        {"main.ts", "import { greet } from './impl'\nconsole.log(greet())"}
      ]

      {:ok, js} = OXC.bundle(files)
      assert js =~ "function greetImpl()"
      refute js =~ "export"
      refute js =~ "import"
      assert run_bundle(js) == "hi\n"
    end

    test "drops bare re-export specifiers" do
      files = [
        {"a.ts", "export const x = 1;"},
        {"b.ts", "import { x } from './a'\nexport { x }"}
      ]

      {:ok, js} = OXC.bundle(files)
      assert js =~ "const x = 1"
      refute Regex.match?(~r/export\s*\{/, js)
    end

    test "handles side-effect-only imports" do
      files = [
        {"setup.ts", "(globalThis as any).ready = true;"},
        {"main.ts", "import './setup'"}
      ]

      {:ok, js} = OXC.bundle(files)
      assert js =~ "globalThis.ready = true"
    end

    test "handles files with .js extension in imports" do
      files = [
        {"a.ts", "export const x = 1;"},
        {"b.ts", "import { x } from './a.js'\n(globalThis as any).val = x;"}
      ]

      {:ok, js} = OXC.bundle(files)
      assert js =~ "const x = 1"
      refute js =~ "import"
    end

    test "returns errors for invalid syntax" do
      files = [{"bad.ts", "const = ;"}]
      {:error, errors} = OXC.bundle(files)
      assert is_list(errors)
      assert length(errors) > 0
    end

    test "handles circular dependencies by appending remaining modules" do
      files = [
        {"a.ts", "import { B } from './b'\nexport class A extends B {}"},
        {"b.ts", "import { A } from './a'\nexport class B extends A {}"}
      ]

      {:ok, code} = OXC.bundle(files)
      assert code =~ "class A"
      assert code =~ "class B"
    end
  end

  describe "bundle/2 runtime correctness" do
    test "isolates module-private bindings across files" do
      files = [
        {"comp_a.js",
         ~S[const _hoisted_1 = { class: "text-red" }; export function render_a() { return _hoisted_1; }]},
        {"comp_b.js",
         ~S[const _hoisted_1 = { class: "text-blue" }; export function render_b() { return _hoisted_1; }]},
        {"entry.js",
         ~S|import { render_a } from "./comp_a.js"; import { render_b } from "./comp_b.js"; console.log(JSON.stringify([render_a(), render_b()]));|}
      ]

      {:ok, js} = OXC.bundle(files)
      assert run_bundle(js) == ~s([{"class":"text-red"},{"class":"text-blue"}]) <> "\n"
    end

    test "supports default imports from default expressions" do
      files = [
        {"answer.ts", "const answer: number = 42; export default answer as number;"},
        {"entry.ts", "import answer from './answer'; console.log(answer);"}
      ]

      {:ok, js} = OXC.bundle(files)
      refute js =~ " as number"
      assert run_bundle(js) == "42\n"
    end

    test "supports aliased imports" do
      files = [
        {"impl.ts", "export function greet() { return 'hi' }"},
        {"entry.ts", "import { greet as hello } from './impl'; console.log(hello());"}
      ]

      {:ok, js} = OXC.bundle(files)
      assert run_bundle(js) == "hi\n"
    end

    test "supports namespace imports" do
      files = [
        {"a.ts", "export const value = 42;"},
        {"entry.ts", "import * as ns from './a'; console.log(ns.value);"}
      ]

      {:ok, js} = OXC.bundle(files)
      assert run_bundle(js) == "42\n"
    end

    test "resolves nested paths without basename collisions" do
      files = [
        {"src/index.ts", "export const src = 1;"},
        {"lib/index.ts", "export const lib = 2;"},
        {"entry.ts",
         "import { src } from './src/index'; import { lib } from './lib/index'; console.log(JSON.stringify([src, lib]));"}
      ]

      {:ok, js} = OXC.bundle(files)
      assert run_bundle(js) == "[1,2]\n"
    end

    test "handles anonymous default exports" do
      files = [
        {"widget.ts", "export default function() { return 'ok' }"},
        {"entry.ts", "import render from './widget'; console.log(render());"}
      ]

      {:ok, js} = OXC.bundle(files)
      assert run_bundle(js) == "ok\n"
    end
  end

  describe "bundle/2 minify option" do
    test "minifies output" do
      files = [{"a.ts", "const longName: number = 42; (globalThis as any).v = longName;"}]

      {:ok, normal} = OXC.bundle(files)
      {:ok, minified} = OXC.bundle(files, minify: true)
      assert byte_size(minified) < byte_size(normal)
    end

    test "folds constants when minifying" do
      files = [{"a.ts", "const x = 1 + 2; (globalThis as any).x = x;"}]
      {:ok, js} = OXC.bundle(files, minify: true)
      assert js =~ "3"
    end

    test "mangles names when minifying" do
      files = [
        {"a.ts",
         "function compute() { const longVariableName = 42; return longVariableName; } (globalThis as any).f = compute;"}
      ]

      {:ok, js} = OXC.bundle(files, minify: true)
      refute js =~ "longVariableName"
    end

    test "tree-shakes unused code when minifying" do
      files = [{"a.ts", "function unused() {} (globalThis as any).x = 1;"}]
      {:ok, js} = OXC.bundle(files, minify: true)
      refute js =~ "unused"
    end
  end

  describe "bundle/2 banner/footer options" do
    test "prepends banner" do
      files = [{"a.ts", "const x = 1;"}]
      {:ok, js} = OXC.bundle(files, banner: "/* MIT License */")
      assert String.starts_with?(js, "/* MIT License */")
    end

    test "appends footer" do
      files = [{"a.ts", "const x = 1;"}]
      {:ok, js} = OXC.bundle(files, footer: "/* end */")
      assert String.ends_with?(String.trim(js), "/* end */")
    end

    test "applies both banner and footer" do
      files = [{"a.ts", "const x = 1;"}]
      {:ok, js} = OXC.bundle(files, banner: "/* top */", footer: "/* bottom */")
      assert String.starts_with?(js, "/* top */")
      assert String.ends_with?(String.trim(js), "/* bottom */")
    end
  end

  describe "bundle/2 define option" do
    test "replaces identifiers" do
      files = [{"a.ts", "const env = process.env.NODE_ENV; (globalThis as any).env = env;"}]

      {:ok, js} =
        OXC.bundle(files, define: %{"process.env.NODE_ENV" => ~s("production")})

      assert js =~ ~s("production")
      refute js =~ "process.env"
    end

    test "replaces nested identifiers" do
      files = [{"a.ts", "if (DEBUG) { console.log('debug mode') }"}]
      {:ok, js} = OXC.bundle(files, define: %{"DEBUG" => "false"})
      # With define, DEBUG becomes false; the if(false) block may remain or be optimized
      refute js =~ "DEBUG"
    end

    test "combined with minify enables dead code elimination" do
      files = [
        {"a.ts",
         "if (process.env.NODE_ENV === 'development') { console.log('dev') } (globalThis as any).x = 1;"}
      ]

      {:ok, js} =
        OXC.bundle(files,
          define: %{"process.env.NODE_ENV" => ~s("production")},
          minify: true
        )

      refute js =~ "dev"
    end
  end

  describe "bundle/2 drop_console option" do
    test "removes console calls when minifying" do
      files = [
        {"a.ts", "console.log('hi'); console.warn('careful'); (globalThis as any).x = 1;"}
      ]

      {:ok, js} = OXC.bundle(files, minify: true, drop_console: true)
      refute js =~ "console"
      assert js =~ "1"
    end
  end

  describe "bundle/2 jsx options" do
    test "transforms JSX with custom pragma" do
      files = [{"app.jsx", "export const App = () => <div>hello</div>"}]
      {:ok, js} = OXC.bundle(files, jsx: :classic, jsx_factory: "h")
      assert js =~ "h("
      refute js =~ "createElement"
    end

    test "transforms JSX with custom fragment" do
      files = [{"app.jsx", "export const App = () => <><span /></>"}]

      {:ok, js} =
        OXC.bundle(files, jsx: :classic, jsx_factory: "h", jsx_fragment: "Fragment")

      assert js =~ "Fragment"
      refute js =~ "React"
    end

    test "defaults to automatic runtime" do
      files = [{"app.jsx", "export const App = () => <div />"}]
      {:ok, js} = OXC.bundle(files)
      assert js =~ "jsx"
      refute js =~ "createElement"
    end
  end

  describe "bundle/2 sourcemap option" do
    test "returns map with code and sourcemap" do
      files = [{"a.ts", "const x: number = 1; (globalThis as any).x = x;"}]
      {:ok, result} = OXC.bundle(files, sourcemap: true)
      assert is_map(result)
      assert is_binary(result.code)
      assert is_binary(result.sourcemap)
    end

    test "sourcemap points to original bundle sources" do
      files = [
        {"a.ts", "export const x = 1;"},
        {"b.ts", "import { x } from './a'; console.log(x);"}
      ]

      {:ok, result} = OXC.bundle(files, sourcemap: true)
      assert {:ok, map} = Jason.decode(result.sourcemap)
      assert map["version"] == 3
      assert Enum.sort(map["sources"]) == ["a.ts", "b.ts"]
      refute "bundle.js" in map["sources"]
    end

    test "sourcemap works with minify" do
      files = [
        {"a.ts", "export const x = 1;"},
        {"b.ts", "import { x } from './a'; console.log(x);"}
      ]

      {:ok, result} = OXC.bundle(files, minify: true, sourcemap: true)
      assert is_binary(result.code)
      assert is_binary(result.sourcemap)
      assert {:ok, map} = Jason.decode(result.sourcemap)
      assert map["version"] == 3
      assert "b.ts" in map["sources"]
      refute "bundle.js" in map["sources"]
    end

    test "returns plain string without sourcemap option" do
      files = [{"a.ts", "const x = 1;"}]
      {:ok, js} = OXC.bundle(files)
      assert is_binary(js)
    end
  end

  describe "bundle/2 target option" do
    test "downlevels with target" do
      files = [{"a.js", "const x = a ?? b; (globalThis).x = x;"}]
      {:ok, js} = OXC.bundle(files, target: "es2019")
      refute js =~ "??"
    end
  end

  describe "bundle!/2" do
    test "returns result on success" do
      files = [{"a.ts", "const x: number = 1;"}]
      js = OXC.bundle!(files)
      assert is_binary(js)
      assert js =~ "const x = 1"
    end

    test "raises on error" do
      files = [{"bad.ts", "const = ;"}]

      assert_raise RuntimeError, ~r/bundle error/, fn ->
        OXC.bundle!(files)
      end
    end

    test "returns map when sourcemap requested" do
      files = [{"a.ts", "const x = 1;"}]
      result = OXC.bundle!(files, sourcemap: true)
      assert is_map(result)
      assert is_binary(result.code)
    end
  end

  defp run_bundle(js) do
    runtime = System.find_executable("bun") || System.find_executable("node")
    assert runtime, "bun or node is required to verify bundle runtime behavior"

    path =
      Path.join(
        System.tmp_dir!(),
        "oxc-bundle-#{System.unique_integer([:positive, :monotonic])}.js"
      )

    try do
      File.write!(path, js)
      {output, 0} = System.cmd(runtime, [path], stderr_to_stdout: true)
      output
    after
      File.rm(path)
    end
  end
end
