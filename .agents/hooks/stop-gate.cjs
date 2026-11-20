const { execFileSync, spawnSync } = require('child_process');
const fs = require('fs');
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

function wasKilledAfter(transcriptPath, stepIdx) {
  if (!transcriptPath || !fs.existsSync(transcriptPath)) return false;
  try {
    const lines = fs.readFileSync(transcriptPath, 'utf8').split('\n');
    for (const line of lines) {
      if (!line.trim()) continue;
      let step;
      try {
        step = JSON.parse(line);
      } catch {
        continue;
      }
      if (typeof step.step_index === 'number' && step.step_index <= stepIdx) continue;
      const calls = Array.isArray(step.tool_calls) ? step.tool_calls : [];
      for (const tc of calls) {
        const name = tc.name || (tc.function && tc.function.name) || '';
        const args = tc.args || tc.arguments || (tc.function && tc.function.arguments) || {};
        if (name === 'manage_subagents' && (args.Action === 'kill' || args.Action === 'kill_all')) {
          return true;
        }
      }
    }
  } catch {}
  return false;
}

function flushPendingReviews(sessionId, transcriptPath) {
  try {
    const gitDir = execFileSync('git', ['rev-parse', '--absolute-git-dir'], {
      cwd: root,
      encoding: 'utf8',
      timeout: 10000,
      stdio: ['ignore', 'pipe', 'ignore'],
    }).trim();
    const dir = path.join(gitDir, 'agent-gate');
    const pendingPath = path.join(dir, `${sessionId}.pending.json`);
    if (!fs.existsSync(pendingPath)) return;
    let pending = [];
    try {
      pending = JSON.parse(fs.readFileSync(pendingPath, 'utf8'));
    } catch {}
    fs.rmSync(pendingPath, { force: true });
    if (!Array.isArray(pending) || !pending.length) return;
    const statePath = path.join(dir, `${sessionId}.json`);
    let state = null;
    try {
      state = JSON.parse(fs.readFileSync(statePath, 'utf8'));
    } catch {}
    if (!state) return;
    state.reviewed = state.reviewed || {};
    let changed = false;
    for (const item of pending) {
      if (!item || !item.type || !item.tree) continue;
      if (wasKilledAfter(transcriptPath, item.stepIdx || 0)) continue;
      state.reviewed[item.type] = item.tree;
      changed = true;
    }
    if (changed) {
      fs.writeFileSync(statePath, JSON.stringify(state));
    }
  } catch {}
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
  if (input.fullyIdle === false) {
    respond({});
  }
  const py = findPython();
  if (!py) {
    respond({
      decision: 'continue',
      reason: 'The gate could not run because Python was not found. Fix the Python PATH or .claude/bin/gate, then stop again.',
    });
  }
  const sessionId = input.conversationId || 'agy-default';
  flushPendingReviews(sessionId, input.transcriptPath);
  const [cmd, prefix] = py;
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
