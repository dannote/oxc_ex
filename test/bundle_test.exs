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

    test "emits alias for renamed export specifiers" do
      files = [
        {"impl.ts", "function greetImpl() { return 'hi' }\nexport { greetImpl as greet }"},
        {"main.ts", "import { greet } from './impl'\n(globalThis as any).g = greet;"}
      ]

      {:ok, js} = OXC.bundle(files)
      assert js =~ "function greetImpl()"
      assert js =~ "var greet = greetImpl"
      refute js =~ "export"
      refute js =~ "import"
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

    test "returns error for circular dependencies" do
      files = [
        {"a.ts", "import { B } from './b'\nexport class A extends B {}"},
        {"b.ts", "import { A } from './a'\nexport class B extends A {}"}
      ]

      {:error, errors} = OXC.bundle(files)
      assert hd(errors) =~ "Circular"
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

    test "sourcemap is valid JSON" do
      files = [{"a.ts", "const x = 1;"}]
      {:ok, result} = OXC.bundle(files, sourcemap: true)
      assert {:ok, map} = Jason.decode(result.sourcemap)
      assert map["version"] == 3
    end

    test "sourcemap works with minify" do
      files = [{"a.ts", "const x = 1; (globalThis as any).x = x;"}]
      {:ok, result} = OXC.bundle(files, minify: true, sourcemap: true)
      assert is_binary(result.code)
      assert is_binary(result.sourcemap)
      assert {:ok, map} = Jason.decode(result.sourcemap)
      assert map["version"] == 3
    end

    test "returns plain string without sourcemap option" do
      files = [{"a.ts", "const x = 1;"}]
      {:ok, js} = OXC.bundle(files)
      assert is_binary(js)
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
end
