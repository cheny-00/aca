// Passed in with --plugin-dir when aca starts Claude (crates/aca-core/src/agent/claude_mod.rs), not installed into any config directory.
// Only forwards: each time Claude measures usage it's handed to aca as-is, and parsing the format happens on aca's side —
// when the interface changes, fix it there and leave this alone as far as possible.
// ACA_BIN is put in the environment when the session starts. Silent on any error: failing to record usage mustn't get in
// the way of the conversation.
export const register = (on) => {
  on('session.measure', async ($, e, next) => {
    try {
      await $.process.run(['/bin/sh', '-c', 'exec "$ACA_BIN" limits --mod'], { stdin: JSON.stringify(e), timeoutMs: 5000 })
    } catch {}
    return next(e)
  })
}
