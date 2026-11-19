#!/usr/bin/env bun
/**
 * Verify how this repository reaches GPUI. Adapted from gpui-kit
 * `script/check-gpui-pin.ts`.
 *
 * gpui-kit pins every `gpui-pre-*` snapshot crate to an exact version, and a
 * new snapshot only reaches applications through a gpui-kit release that bumps
 * the pin. This repository depends on gpui-kit, so it must:
 *
 * 1. depend on `gpui-kit` from crates.io on one minor release line (`0.10`,
 *    `^0.10.2`, `~1.4` or an exact `=x.y.z`): each minor release pins a new
 *    GPUI snapshot, and `Cargo.lock` holds the exact release. A git `rev` (a
 *    full 40-hex sha, not a branch or tag) is accepted too, to try an
 *    unreleased change. A `path` dependency is accepted locally with a
 *    warning and rejected in CI (`CI` environment variable set);
 * 2. not list `gpui-pre*` crates, except crates gpui-kit does not re-export;
 *    each of those must be an exact `=x.y.z` requirement, all on one version;
 * 3. resolve exactly one version of `gpui-pre` in `Cargo.lock` (when it exists),
 *    equal to the version of rule 2 if any. Two versions mean the direct
 *    snapshot crates drifted from the snapshot gpui-kit was built against;
 * 4. record `gpui-kit` in `Cargo.lock` from crates.io (or the pinned git
 *    revision). A local `[patch]` (`.cargo/config.local.toml`) rewrites it as
 *    a path entry; that is accepted locally with a warning and rejected in CI.
 *
 * The manifest is a workspace root (`[workspace.dependencies]`) or a package
 * with its own workspace (`[dependencies]`, as `crates/grid-web`).
 *
 *     bun script/check-gpui-kit-pin.ts [path/to/Cargo.toml]
 */
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";

const REPO_ROOT = resolve(import.meta.dir, "..");
const MANIFEST = "Cargo.toml";
const KIT = "gpui-kit";
const SNAPSHOT_PREFIX = "gpui-pre";
const EXACT_VERSION = /^=(\d+\.\d+\.\d+)$/;
/** One minor release line: `0.y` or `^0.y.z` (y > 0), `~x.y`, `=x.y.z`. */
const ONE_MINOR = /^(?:\^?0\.[1-9]\d*(?:\.\d+)?|~\d+\.\d+(?:\.\d+)?|=\d+\.\d+\.\d+)$/;
const FULL_SHA = /^[0-9a-f]{40}$/;
const CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index";

type Spec = Record<string, unknown>;

function manifestDependencies(manifestPath: string): Record<string, unknown> {
  const manifest = Bun.TOML.parse(readFileSync(manifestPath, "utf8")) as {
    workspace?: { dependencies?: Record<string, unknown> };
    dependencies?: Record<string, unknown>;
  };
  return { ...(manifest.dependencies ?? {}), ...(manifest.workspace?.dependencies ?? {}) };
}

function packageName(alias: string, spec: unknown): string {
  if (typeof spec === "object" && spec !== null && typeof (spec as Spec).package === "string") {
    return (spec as Spec).package as string;
  }
  return alias;
}

/** Checks a registry requirement on `gpui-kit`; returns the source it resolves from. */
function checkKitVersion(version: unknown, problems: string[]): string | undefined {
  if (typeof version !== "string" || !ONE_MINOR.test(version)) {
    problems.push(
      `\`${KIT}\` version ${JSON.stringify(version ?? "")} is not on one minor release line ` +
        `(\`0.y\`, \`~x.y\` or \`=x.y.z\`); each minor release pins a new GPUI snapshot`,
    );
    return;
  }
  return CRATES_IO;
}

/** Checks the `gpui-kit` requirement; returns the source `Cargo.lock` must record. */
function checkKit(dependencies: Record<string, unknown>, problems: string[], warnings: string[]): string | undefined {
  const entry = Object.entries(dependencies).find(([alias, spec]) => packageName(alias, spec) === KIT);
  if (entry === undefined) {
    problems.push(`no \`${KIT}\` dependency in [workspace.dependencies] or [dependencies]`);
    return;
  }
  const spec = entry[1];
  if (typeof spec === "string") return checkKitVersion(spec, problems);
  const { path, git, rev, branch, tag, version } = spec as Spec;
  if (typeof path === "string") {
    const message = `\`${KIT}\` is a path dependency (${path}); depend on a release (or a git \`rev\`) before CI`;
    if (process.env.CI) problems.push(message);
    else warnings.push(message);
    return;
  }
  if (typeof git === "string") {
    if (branch !== undefined || tag !== undefined) problems.push(`\`${KIT}\` git dependency uses a branch or tag; pin \`rev\``);
    if (typeof rev !== "string" || !FULL_SHA.test(rev)) {
      problems.push(`\`${KIT}\` git dependency must pin \`rev\` to a full 40-hex sha`);
      return;
    }
    return `git+${git}?rev=${rev}#${rev}`;
  }
  return checkKitVersion(version, problems);
}

function checkSnapshots(dependencies: Record<string, unknown>, problems: string[]): string | undefined {
  const versions = new Map<string, string[]>();
  for (const [alias, spec] of Object.entries(dependencies)) {
    const name = packageName(alias, spec);
    if (!name.startsWith(SNAPSHOT_PREFIX)) continue;
    if (name === SNAPSHOT_PREFIX || name === `${SNAPSHOT_PREFIX}-platform`) {
      problems.push(`\`${name}\` must come through \`${KIT}\` (\`gpui_kit::*\`, \`gpui_kit::platform\`), not as a direct dependency`);
    }
    const version = typeof spec === "string" ? spec : (spec as Spec).version;
    const exact = typeof version === "string" ? EXACT_VERSION.exec(version) : null;
    if (exact === null) {
      problems.push(`\`${name}\` is not an exact \`=x.y.z\` requirement`);
      continue;
    }
    versions.set(exact[1], [...(versions.get(exact[1]) ?? []), name]);
  }
  if (versions.size > 1) {
    const detail = [...versions].map(([v, names]) => `${v} (${names.join(", ")})`).join(", ");
    problems.push(`the snapshot crates are pinned to more than one version: ${detail}`);
  }
  return versions.size === 1 ? [...versions.keys()][0] : undefined;
}

function checkLock(
  lockPath: string,
  pinned: string | undefined,
  kitSource: string | undefined,
  problems: string[],
  warnings: string[],
) {
  if (!existsSync(lockPath)) return;
  const lock = Bun.TOML.parse(readFileSync(lockPath, "utf8")) as {
    package?: { name: string; version: string; source?: string }[];
  };
  const packages = lock.package ?? [];
  const resolved = [...new Set(packages.filter((p) => p.name === SNAPSHOT_PREFIX).map((p) => p.version))];
  if (resolved.length > 1) problems.push(`Cargo.lock resolves several \`${SNAPSHOT_PREFIX}\` versions: ${resolved.join(", ")}`);
  if (pinned !== undefined && resolved.length === 1 && resolved[0] !== pinned) {
    problems.push(`direct snapshot crates pin ${pinned}, but gpui-kit resolves \`${SNAPSHOT_PREFIX}\` ${resolved[0]}`);
  }
  if (kitSource === undefined) return;
  const kit = packages.find((p) => p.name === KIT);
  if (kit === undefined || kit.source === kitSource) return;
  if (kit.source === undefined) {
    const message = `Cargo.lock records \`${KIT}\` as a path package (a local \`[patch]\`); restore the released entries before committing`;
    if (process.env.CI) problems.push(message);
    else warnings.push(message);
    return;
  }
  problems.push(`Cargo.lock records \`${KIT}\` from ${kit.source}, not from ${kitSource}`);
}

const manifestPath = resolve(process.argv[2] ?? join(REPO_ROOT, MANIFEST));
const label = relative(REPO_ROOT, manifestPath);
const dependencies = manifestDependencies(manifestPath);
const problems: string[] = [];
const warnings: string[] = [];
const kitSource = checkKit(dependencies, problems, warnings);
const pinned = checkSnapshots(dependencies, problems);
checkLock(join(dirname(manifestPath), "Cargo.lock"), pinned, kitSource, problems, warnings);

for (const warning of warnings) console.warn(`::warning file=${label}::${warning}`);
if (problems.length > 0) {
  for (const problem of problems) console.error(`::error file=${label}::${problem}`);
  console.error(`\n${label} must reach GPUI only through a pinned \`${KIT}\` release.`);
  process.exit(1);
}
console.log(`${label} reaches GPUI through a pinned \`${KIT}\``);
