#!/usr/bin/env node
// Build the `nagori` CLI and stage it as a Tauri `externalBin` sidecar so it
// rides inside the desktop bundle (macOS: `Nagori.app/Contents/MacOS/nagori`).
//
// Tauri resolves `externalBin` entries by appending the *target triple* to the
// configured base name, so this script copies the freshly built binary to
// `apps/desktop/src-tauri/binaries/nagori-<triple>[.exe]`. The triple defaults
// to the host (release runners build each platform natively, so host == target
// there); pass `--target <triple>` or set `NAGORI_CLI_TARGET` to override when
// cross-compiling.
//
// `beforeBuildCommand` runs this on the bundle path (release profile). Pass
// `--debug` to stage a dev-profile binary instead, e.g. when manually testing
// the in-app installer against a debug `tauri build`.
import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..');
const argv = process.argv.slice(2);
const debug = argv.includes('--debug');

const targetFlagIndex = argv.indexOf('--target');
const target =
  (targetFlagIndex >= 0 ? argv[targetFlagIndex + 1] : undefined) ??
  process.env.NAGORI_CLI_TARGET ??
  hostTriple();

function hostTriple() {
  const out = execFileSync('rustc', ['-vV'], { encoding: 'utf8' });
  const match = out.match(/^host:\s*(.+)$/m);
  if (!match) {
    throw new Error('could not determine host triple from `rustc -vV`');
  }
  return match[1].trim();
}

const cargoArgs = ['build', '--package', 'nagori-cli', '--target', target];
if (!debug) {
  cargoArgs.push('--release');
}
execFileSync('cargo', cargoArgs, { cwd: repoRoot, stdio: 'inherit' });

const exeSuffix = target.includes('windows') ? '.exe' : '';
const profileDir = debug ? 'debug' : 'release';
const builtBinary = join(repoRoot, 'target', target, profileDir, `nagori${exeSuffix}`);

const outputDir = join(repoRoot, 'apps', 'desktop', 'src-tauri', 'binaries');
mkdirSync(outputDir, { recursive: true });
const sidecar = join(outputDir, `nagori-${target}${exeSuffix}`);
copyFileSync(builtBinary, sidecar);
console.log(`staged CLI sidecar: ${sidecar}`);
