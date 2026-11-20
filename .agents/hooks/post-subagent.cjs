const { execFileSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const root = path.resolve(__dirname, '..', '..');
const REVIEWERS = new Set(['rust-reviewer', 'ui-auditor', 'ux-tester', 'perf-tester']);

function git(args, env) {
  return execFileSync('git', args, {
    cwd: root,
    env: env || process.env,
    encoding: 'utf8',
    timeout: 15000,
    stdio: ['ignore', 'pipe', 'ignore'],
  }).trim();
}

function snapshotTree() {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'mc-snap-'));
  try {
    const idx = path.join(tmp, 'index');
    const real = path.resolve(root, git(['rev-parse', '--git-path', 'index']));
    if (fs.existsSync(real)) fs.copyFileSync(real, idx);
    const env = { ...process.env, GIT_INDEX_FILE: idx };
    git(['add', '-A'], env);
    return git(['write-tree'], env);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
}

let raw = '';
process.stdin.on('data', (c) => (raw += c));
process.stdin.on('end', () => {
  try {
    const input = JSON.parse(raw || '{}');
    const args = (input.toolCall && input.toolCall.args) || {};
    const agentType = args.TypeName || '';
    if (!input.error && REVIEWERS.has(agentType)) {
      const sessionId = input.conversationId || 'agy-default';
      const gitDir = git(['rev-parse', '--absolute-git-dir']);
      const dir = path.join(gitDir, 'agent-gate');
      fs.mkdirSync(dir, { recursive: true });
      const pendingPath = path.join(dir, `${sessionId}.pending.json`);
      let list = [];
      try {
        list = JSON.parse(fs.readFileSync(pendingPath, 'utf8'));
        if (!Array.isArray(list)) list = [];
      } catch {}
      list.push({
        type: agentType,
        stepIdx: typeof input.stepIdx === 'number' ? input.stepIdx : 0,
        tree: snapshotTree(),
      });
      fs.writeFileSync(pendingPath, JSON.stringify(list));
    }
  } catch {}
  process.stdout.write('{}\n');
  process.exit(0);
});
