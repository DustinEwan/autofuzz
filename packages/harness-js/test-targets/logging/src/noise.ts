// The targets the protocol-channel test invokes. Real packages log: `yaml`'s exported `debug` is
// `console.log(...messages)`, and the plugin's stdout is its JSON-RPC channel, so what a target writes
// there is read back by the engine as a protocol message.
export function logNoise(text: string): string {
  console.log(`noise ${text}`);
  return text;
}

/** Write to the stdout stream directly, the way a CLI-shaped target would. */
export function writeStdout(text: string): string {
  process.stdout.write(`raw ${text}\n`);
  return text;
}
