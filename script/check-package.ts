#!/usr/bin/env bun
/**
 * Check what crates.io would get: the files `cargo package --list` puts in
 * the two published crates, and their versions.
 *
 * 1. `gpui-kit-grid` holds its sources, translations and the layout fixtures
 *    its unit tests read, with the README and the licence; `gpui-kit-grid-macros`
 *    its sources and the licence. Any other file fails: the examples and the
 *    integration tests need crates that are not published.
 * 2. Both crates have one version, and the workspace requires the macros at
 *    exactly that version (`=x.y.z`).
 *
 *     bun script/check-package.ts
 */
import { readFileSync } from "node:fs";
import { join, resolve } from "node:path";

const REPO_ROOT = resolve(import.meta.dir, "..");

// cargo's own files, in every package.
const CARGO = [/^\.cargo_vcs_info\.json$/, /^Cargo\.toml(\.orig)?$/, /^Cargo\.lock$/];

const CRATES = [
  {
    name: "gpui-kit-grid",
    manifest: "crates/grid/Cargo.toml",
    required: ["Cargo.toml", "README.md", "LICENSE-APACHE", "src/lib.rs", "locales/live_grid.yml"],
    allowed: [
      ...CARGO,
      /^README\.md$/,
      /^LICENSE-APACHE$/,
      /^src\/.+\.rs$/,
      /^locales\/[^/]+\.yml$/,
      /^tests\/fixtures\/[^/]+\.json$/,
    ],
  },
  {
    name: "gpui-kit-grid-macros",
    manifest: "crates/grid-macros/Cargo.toml",
    required: ["Cargo.toml", "LICENSE-APACHE", "src/lib.rs"],
    allowed: [...CARGO, /^LICENSE-APACHE$/, /^src\/.+\.rs$/],
  },
];

function packageFiles(name: string): string[] {
  const run = Bun.spawnSync(["cargo", "package", "--list", "--allow-dirty", "--locked", "-p", name], {
    cwd: REPO_ROOT,
    stderr: "pipe",
  });
  if (run.exitCode !== 0) {
    console.error(run.stderr.toString());
    throw new Error(`cargo package --list -p ${name} failed`);
  }
  return run.stdout.toString().split("\n").filter(Boolean);
}

function version(manifest: string): string {
  const parsed = Bun.TOML.parse(readFileSync(join(REPO_ROOT, manifest), "utf8")) as {
    package: { version: string };
  };
  return parsed.package.version;
}

const problems: string[] = [];
for (const crate of CRATES) {
  const files = packageFiles(crate.name);
  for (const file of crate.required) {
    if (!files.includes(file)) problems.push(`${crate.name}: ${file} is missing from the package`);
  }
  for (const file of files) {
    if (!crate.allowed.some((pattern) => pattern.test(file))) {
      problems.push(`${crate.name}: ${file} is not meant for crates.io (\`include\` in ${crate.manifest})`);
    }
  }
  console.log(`${crate.name}: ${files.length} files`);
}

const [grid, macros] = CRATES.map((crate) => version(crate.manifest));
if (grid !== macros) problems.push(`gpui-kit-grid is ${grid}, gpui-kit-grid-macros ${macros}: one version for both`);
const workspace = Bun.TOML.parse(readFileSync(join(REPO_ROOT, "Cargo.toml"), "utf8")) as {
  workspace: { dependencies: Record<string, { version?: string }> };
};
const required = workspace.workspace.dependencies["gpui-kit-grid-macros"]?.version;
if (required !== `=${macros}`) {
  problems.push(`the workspace requires gpui-kit-grid-macros ${JSON.stringify(required)}, not "=${macros}"`);
}

if (problems.length > 0) {
  for (const problem of problems) console.error(`::error::${problem}`);
  process.exit(1);
}
console.log("the packages hold what crates.io should get");
