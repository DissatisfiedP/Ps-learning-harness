// REPL driver for the `repl` built-in plugin (crates/harness/src/plugins/native/repl.rs).
// One JSON request per stdin line: {"op": "eval" | "check" | "syntax", "code": "..."}.
// Printed output goes to stdout as it happens; each request ends with
// RES + one JSON line, then END on its own line.
'use strict';
const vm = require('node:vm');
const util = require('node:util');
const readline = require('node:readline');

const RES = '\x1eLHRES\x1e';
const END = '\x1eLHEND\x1e';
const write = (s) => process.stdout.write(s);
const show = (v) => util.inspect(v, { depth: 4, breakLength: 100 });
const log = (...args) => write(args.map((a) => (typeof a === 'string' ? a : show(a))).join(' ') + '\n');
const sandbox = {
  console: { log, info: log, warn: log, error: log, debug: log },
};
const context = vm.createContext(sandbox);

function reply(payload) {
  write(RES + JSON.stringify(payload) + '\n' + END + '\n');
}

function error(e) {
  if (e && typeof e === 'object' && 'stack' in e) {
    return String(e.stack)
      .split('\n')
      .filter((l) => !l.includes('node:') && !l.includes('driver.js'))
      .join('\n');
  }
  return 'Uncaught ' + show(e);
}

const rl = readline.createInterface({ input: process.stdin, terminal: false });
rl.on('line', (line) => {
  let req;
  try {
    req = JSON.parse(line);
  } catch {
    return;
  }
  const code = String(req.code ?? '');
  try {
    if (req.op === 'syntax') {
      new vm.Script('(' + code + '\n)', { filename: 'check' });
      reply({ ok: true });
    } else if (req.op === 'check') {
      const value = vm.runInContext('(' + code + '\n)', context, { filename: 'check', timeout: 5000 });
      reply({ ok: true, value: show(value), truthy: Boolean(value) });
    } else {
      const value = vm.runInContext(code, context, { filename: 'repl' });
      if (value !== undefined) sandbox._ = value;
      reply({ ok: true, value: value === undefined ? null : show(value) });
    }
  } catch (e) {
    reply({ ok: false, error: error(e) });
  }
});
