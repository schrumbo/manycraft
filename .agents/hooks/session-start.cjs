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

let raw = '';
process.stdin.on('data', (c) => (raw += c));
process.stdin.on('end', () => {
  let input = {};
  try {
    input = JSON.parse(raw || '{}');
  } catch {}
  const py = findPython();
  if (!py) {
    process.stdout.write('{}\n');
    process.exit(0);
  }
  const [cmd, prefix] = py;
  const syncRes = spawnSync(cmd, [...prefix, path.join(root, '.claude', 'bin', 'sync')], {
    cwd: root,
    env,
    encoding: 'utf8',
    timeout: 90000,
    stdio: ['ignore', 'pipe', 'ignore'],
  });
  const sessionId = input.conversationId || 'agy-default';
  spawnSync(cmd, [...prefix, path.join(root, '.claude', 'bin', 'gate'), '--init'], {
    cwd: root,
    env,
    input: JSON.stringify({ session_id: sessionId }),
    encoding: 'utf8',
    timeout: 20000,
    stdio: ['pipe', 'ignore', 'ignore'],
  });
  const msg = (syncRes.stdout || '').trim();
  const out = msg ? { injectSteps: [{ systemMessage: { systemMessage: msg } }] } : {};
  process.stdout.write(JSON.stringify(out) + '\n');
  process.exit(0);
});
