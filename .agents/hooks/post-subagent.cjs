const { spawnSync } = require('child_process');
const path = require('path');

const root = path.resolve(__dirname, '..', '..');
const REVIEWERS = new Set(['rust-reviewer', 'ui-auditor', 'ux-tester', 'perf-tester']);

function findPython() {
  const candidates =
    process.platform === 'win32'
      ? [
          ['python', []],
          ['py', ['-3']],
          ['python3', []],
        ]
      : [
          ['python3', []],
          ['python', []],
        ];
  for (const [cmd, prefix] of candidates) {
    const res = spawnSync(cmd, [...prefix, '--version'], { encoding: 'utf8', timeout: 3000, stdio: 'pipe' });
    if (res.status === 0) return [cmd, prefix];
  }
  return null;
}

let raw = '';
process.stdin.on('data', (c) => (raw += c));
process.stdin.on('end', () => {
  try {
    const input = JSON.parse(raw || '{}');
    const args = (input.toolCall && input.toolCall.args) || {};
    const agentType = args.TypeName || '';
    if (!input.error && REVIEWERS.has(agentType)) {
      const py = findPython();
      if (py) {
        const [cmd, prefix] = py;
        spawnSync(cmd, [...prefix, path.join(root, '.claude', 'bin', 'gate'), '--mark'], {
          cwd: root,
          input: JSON.stringify({ session_id: input.conversationId || 'agy-default', agent_type: agentType }),
          encoding: 'utf8',
          timeout: 20000,
          stdio: ['pipe', 'ignore', 'ignore'],
        });
      }
    }
  } catch {}
  process.stdout.write('{}\n');
  process.exit(0);
});
