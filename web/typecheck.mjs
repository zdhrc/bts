import { execFileSync } from 'node:child_process';
import { resolve } from 'node:path';
const root = resolve(import.meta.dirname, '..');
const version = execFileSync(resolve(root, 'target/debug/bts'), ['--version'], { encoding: 'utf8' }).trim().split(' ')[1];
execFileSync(resolve(import.meta.dirname, 'node_modules/.bin/tsc'), [
  '--noEmit', '--strict', '--target', 'ES2022', '--module', 'NodeNext',
  resolve(root, `target/wasm/bts-v${version}-wasm/example.ts`),
], { stdio: 'inherit' });
