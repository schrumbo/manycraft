const { spawnSync } = require('child_process');
const path = require('path');

const root = path.resolve(__dirname, '..', '..');
const env = {
  ...process.env,
  CARGO_TERM_QUIET: 'true',
  CARGO_TERM_COLOR: 'never',
  CARGO_TERM_PROGRESS_WHEN: 'never',
  RUST_BACKTRACE: '1',
};

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

function respond(obj) {
  process.stdout.write(JSON.stringify(obj) + '\n');
  process.exit(0);
}

let raw = '';
process.stdin.on('data', (c) => (raw += c));
process.stdin.on('end', () => {
  let input = {};
  try {
    input = JSON.parse(raw || '{}');
  } catch {}
  if (input.terminationReason && input.terminationReason !== 'NO_TOOL_CALL') {
    respond({});
  }
  const py = findPython();
  if (!py) {
    respond({
      decision: 'continue',
      reason: 'The gate could not run because Python was not found. Fix the Python PATH or .claude/bin/gate, then stop again.',
    });
  }
  const [cmd, prefix] = py;
  const sessionId = input.conversationId || 'agy-default';
  const res = spawnSync(cmd, [...prefix, path.join(root, '.claude', 'bin', 'gate'), '--hook'], {
    cwd: root,
    env,
    input: JSON.stringify({ session_id: sessionId }),
    encoding: 'utf8',
    timeout: 570000,
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  if (res.status !== 0) {
    respond({
      decision: 'continue',
      reason: 'The gate could not run, so nothing was checked. Fix .claude/bin/gate, then stop again.',
    });
  }
  const out = (res.stdout || '').trim();
  if (!out) respond({});
  try {
    const parsed = JSON.parse(out);
    if (parsed.decision === 'block') {
      respond({ decision: 'continue', reason: parsed.reason || 'Quality gate blocked stop.' });
    }
  } catch {}
  respond({});
});
