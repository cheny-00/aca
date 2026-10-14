// aca 起 Claude 时用 --plugin-dir 带进来的(crates/aca-core/src/agent/claude_mod.rs),不装进任何配置目录。
// 只转发:Claude 每次量出用量就原样交给 aca,认格式在 aca 那边 —— 接口变了改那边,这里尽量不动。
// ACA_BIN 是起会话时放进环境的。出什么错都不吭声:用量记不上不该碍着对话。
export const register = (on) => {
  on('session.measure', async ($, e, next) => {
    try {
      await $.process.run(['/bin/sh', '-c', 'exec "$ACA_BIN" limits --mod'], { stdin: JSON.stringify(e), timeoutMs: 5000 })
    } catch {}
    return next(e)
  })
}
