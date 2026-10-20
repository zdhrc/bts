// Typechecked against the built, version-matched declarations.
import init, { version, languageReference, check, generate } from './bts.js';
import type { GenerationOptions, EventBatch, BtsError } from './bts.js';

await init();
console.log(version(), languageReference());
const source = 'trace "example" { output = uuid() }';
const options: GenerationOptions = { count: 5, seed: '42', start: '2026-09-01T00:00:00Z', end: '2026-09-01T01:00:00Z' };
console.log(check(source));
try {
  const result: EventBatch = generate(source, options);
  console.log(result.events);
} catch (error) {
  console.error((error as BtsError).diagnostics);
}
