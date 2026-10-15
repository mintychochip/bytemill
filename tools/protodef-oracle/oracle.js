#!/usr/bin/env node
// protodef-oracle: compile a minecraft-data protocol.json exactly the way
// node-minecraft-protocol does and exchange raw packet bytes over JSON Lines.
//
//   node oracle.js list  --protocol <protocol.json>
//   node oracle.js gen   --protocol <protocol.json> [--count N=20] [--seed S=1]
//                       [--only <state>.<direction>.<packet>] [--depth D=3]
//   node oracle.js check --protocol <protocol.json> [--stdin | --file f.jsonl]
//
// gen emits one JSON object per line on stdout:
//   {"state","direction","name","id","hex"}   (hex = payload AFTER the packet-id varint)
// Packets that fail the parse/re-serialise self-check are NOT emitted; they
// are counted and reported on stderr as {"unstable": ...}.
// check reads {"state","direction","id"?,"name"?,"hex"} lines, parses them
// with the compiled codec (on a copy of the buffer), requires full
// consumption and byte-identical re-serialisation. Failures go to stdout as
// {"ok":false,...} lines, a human summary to stderr; exit code 1 on failure.
'use strict'

const fs = require('fs')
const readline = require('readline')
const varint = require('protodef').types.varint
const protoLib = require('./lib/proto')
const { Generator, buildPacketValue } = require('./lib/gen')

function usage (code) {
  process.stderr.write(
    'usage:\n' +
    '  node oracle.js list  --protocol <protocol.json> [--patches <patch.json>]\n' +
    '  node oracle.js gen   --protocol <protocol.json> [--patches <patch.json>] [--count N=20] [--seed S=1]\n' +
    '                       [--only <state>.<direction>.<packet>] [--depth D=3]\n' +
    '  node oracle.js check --protocol <protocol.json> [--patches <patch.json>] [--stdin | --file f.jsonl]\n' +
    '\n' +
    '--patches applies a schemas/versions/patches/*.json file to the description first, as\n' +
    '`bpsc import-versions` does, so both sides work from the same corrected types.\n')
  process.exit(code)
}

function parseArgs (argv) {
  const mode = argv[2]
  const opts = { count: 20, seed: 1, depth: 3, stdin: false, file: null, only: null, protocol: null, patches: null }
  for (let i = 3; i < argv.length; i++) {
    const a = argv[i]
    if (a === '--protocol') opts.protocol = argv[++i]
    else if (a === '--patches') opts.patches = argv[++i]
    else if (a === '--count') opts.count = parseInt(argv[++i], 10)
    else if (a === '--seed') opts.seed = parseInt(argv[++i], 10)
    else if (a === '--only') opts.only = argv[++i]
    else if (a === '--depth') opts.depth = parseInt(argv[++i], 10)
    else if (a === '--stdin') opts.stdin = true
    else if (a === '--file') opts.file = argv[++i]
    else if (a === '--help' || a === '-h') usage(0)
    else {
      process.stderr.write(`unknown argument: ${a}\n`)
      usage(2)
    }
  }
  return { mode, opts }
}

// Iterate parse -> re-serialise until the bytes stop changing (lossy codecs
// such as lpVec3 quantisation converge to a fixpoint). Returns the stable
// bytes, or null when no fixpoint is reached.
function stabilise (proto, full, maxIters) {
  let cur = full
  for (let i = 0; i < maxIters; i++) {
    // NB: topBitSetTerminatedArray read mutates its input; always parse a copy.
    const parsed = proto.parsePacketBuffer('packet', Buffer.from(cur))
    const next = proto.createPacketBuffer('packet', parsed.data)
    if (next.equals(cur)) return cur
    cur = next
  }
  return null
}

function cmdList (protocolFile, protocolJson) {
  for (const [state, direction] of protoLib.scopesOf(protocolJson)) {
    if (!protoLib.hasPacketType(protocolJson, state, direction)) continue
    const scope = protoLib.getScope(protocolFile, protocolJson, state, direction)
    for (const { id, name } of protoLib.packetTable(scope)) {
      process.stdout.write(`${state} ${direction} 0x${id.toString(16)} ${name}\n`)
    }
  }
}

function cmdGen (protocolFile, protocolJson, opts) {
  let only = null
  if (opts.only) {
    const parts = opts.only.split('.')
    if (parts.length !== 3) {
      process.stderr.write(`--only must be <state>.<direction>.<packet>, got '${opts.only}'\n`)
      process.exit(2)
    }
    only = { state: parts[0], direction: parts[1], name: parts[2] }
  }
  const gen = new Generator(null, opts.seed, { depth: opts.depth })
  let generated = 0
  let unstable = 0
  for (const [state, direction] of protoLib.scopesOf(protocolJson)) {
    if (only && (only.state !== state || only.direction !== direction)) continue
    if (!protoLib.hasPacketType(protocolJson, state, direction)) continue
    const scope = protoLib.getScope(protocolFile, protocolJson, state, direction)
    gen.table = scope.table
    gen.probeProto = scope.proto
    gen.probeMap = scope.probeMap
    let rows = protoLib.packetTable(scope)
    if (only) {
      rows = rows.filter((r) => r.name === only.name)
      if (rows.length === 0) {
        process.stderr.write(`no packet '${only.name}' in ${state}.${direction}\n`)
        process.exit(2)
      }
    }
    for (const { id, name, arm } of rows) {
      for (let n = 0; n < opts.count; n++) {
        let line = null
        let lastError = null
        for (let attempt = 0; attempt < 8 && line === null; attempt++) {
          try {
            const value = buildPacketValue(gen, arm, name)
            const full = scope.proto.createPacketBuffer('packet', value)
            const stable = stabilise(scope.proto, full, 4)
            if (stable === null) {
              lastError = new Error('no parse/re-serialise fixpoint')
              lastError.phase = 'fixpoint'
              continue
            }
            const { value: gotId, size: idSize } = varint[0](stable, 0)
            if (gotId !== id) {
              lastError = new Error(`id mismatch: mapper wrote ${gotId}, table says ${id}`)
              lastError.phase = 'id'
              continue
            }
            line = JSON.stringify({
              state,
              direction,
              name,
              id,
              hex: stable.slice(idSize).toString('hex')
            })
          } catch (e) {
            lastError = e
          }
        }
        if (line === null) {
          unstable++
          const reason = (lastError && lastError.phase) || 'generate'
          process.stderr.write(JSON.stringify({
            unstable: true,
            state,
            direction,
            name,
            id,
            reason,
            detail: String((lastError && lastError.message) || lastError).slice(0, 300)
          }) + '\n')
        } else {
          generated++
          process.stdout.write(line + '\n')
        }
      }
    }
  }
  process.stderr.write(`gen: packets=${generated} unstable=${unstable}\n`)
}

function resolveCheckPacket (scope, table, obj) {
  const byId = new Map(table.map((r) => [r.id, r]))
  const byName = new Map(table.map((r) => [r.name, r]))
  if (obj.id !== undefined && obj.name !== undefined) {
    const row = byId.get(obj.id)
    if (!row) return { error: 'unknown-packet', detail: `id ${obj.id} not in ${scope.state}.${scope.direction} mapper` }
    if (row.name !== obj.name) {
      return { error: 'id-name-mismatch', detail: `id ${obj.id} is '${row.name}', not '${obj.name}'` }
    }
    return { row }
  }
  if (obj.id !== undefined) {
    const row = byId.get(obj.id)
    if (!row) return { error: 'unknown-packet', detail: `id ${obj.id} not in ${scope.state}.${scope.direction} mapper` }
    return { row }
  }
  if (obj.name !== undefined) {
    const row = byName.get(obj.name)
    if (!row) return { error: 'unknown-packet', detail: `name '${obj.name}' not in ${scope.state}.${scope.direction} mapper` }
    return { row }
  }
  return { error: 'unknown-packet', detail: 'neither id nor name given' }
}

function isPartialRead (e) {
  let p = e
  while (p) {
    if (p.name === 'PartialReadError') return true
    p = p.cause
  }
  return /PartialReadError|Unexpected buffer end|Missing characters/i.test(String(e && e.message))
}

async function cmdCheck (protocolFile, protocolJson, opts) {
  if (opts.file && !fs.existsSync(opts.file)) {
    process.stderr.write(`input file not found: ${opts.file}\n`)
    process.exit(2)
  }
  const input = opts.file ? fs.createReadStream(opts.file) : process.stdin
  const rl = readline.createInterface({ input, crlfDelay: Infinity })
  const tables = new Map() // `${state}.${direction}` -> packet table
  function tableFor (state, direction) {
    const key = `${state}.${direction}`
    let t = tables.get(key)
    if (!t) {
      const scope = protoLib.getScope(protocolFile, protocolJson, state, direction)
      t = { scope, rows: protoLib.packetTable(scope) }
      tables.set(key, t)
    }
    return t
  }

  let ok = 0
  let fail = 0
  let lineno = 0
  const reasons = {}
  function failure (obj, scope, row, reason, detail) {
    fail++
    reasons[reason] = (reasons[reason] || 0) + 1
    process.stdout.write(JSON.stringify({
      ok: false,
      state: obj.state,
      direction: obj.direction,
      name: row ? row.name : obj.name,
      id: row ? row.id : obj.id,
      reason,
      detail: String(detail).slice(0, 300),
      hex: typeof obj.hex === 'string' ? obj.hex : undefined
    }) + '\n')
  }

  for await (const line of rl) {
    if (!line.trim()) continue
    lineno++
    let obj
    try {
      obj = JSON.parse(line)
    } catch (e) {
      fail++
      reasons['bad-json'] = (reasons['bad-json'] || 0) + 1
      process.stdout.write(JSON.stringify({ ok: false, line: lineno, reason: 'bad-json', detail: String(e.message).slice(0, 200) }) + '\n')
      continue
    }
    if (!obj.state || !obj.direction || typeof obj.hex !== 'string' || !/^(?:[0-9a-fA-F]{2})*$/.test(obj.hex)) {
      failure(obj, null, null, 'bad-input', 'need {state, direction, hex} with even-length hex')
      continue
    }
    if (!protoLib.hasPacketType(protocolJson, obj.state, obj.direction)) {
      failure(obj, null, null, 'unknown-scope', `no packet type in ${obj.state}.${obj.direction}`)
      continue
    }
    let scope
    let table
    try {
      ({ scope, rows: table } = tableFor(obj.state, obj.direction))
    } catch (e) {
      failure(obj, null, null, 'compile', e.message)
      continue
    }
    const resolved = resolveCheckPacket(scope, table, obj)
    if (resolved.error) {
      failure(obj, scope, null, resolved.error, resolved.detail)
      continue
    }
    const { row } = resolved
    let payload
    try {
      payload = Buffer.from(obj.hex, 'hex')
    } catch (e) {
      failure(obj, scope, row, 'bad-input', e.message)
      continue
    }
    const idBuf = Buffer.alloc(8)
    const idEnd = varint[1](row.id, idBuf, 0)
    const full = Buffer.concat([idBuf.slice(0, idEnd), payload])
    let parsed
    try {
      // NB: parse a copy; topBitSetTerminatedArray read mutates its input.
      parsed = scope.proto.parsePacketBuffer('packet', Buffer.from(full))
    } catch (e) {
      failure(obj, scope, row, isPartialRead(e) ? 'short' : 'parse', e.message)
      continue
    }
    if (parsed.metadata.size !== full.length) {
      failure(obj, scope, row, 'trailing', `consumed ${parsed.metadata.size} of ${full.length} bytes`)
      continue
    }
    let reser
    try {
      reser = scope.proto.createPacketBuffer('packet', parsed.data)
    } catch (e) {
      failure(obj, scope, row, 'reserialise', e.message)
      continue
    }
    if (!reser.equals(full)) {
      let at = 0
      while (at < Math.min(reser.length, full.length) && reser[at] === full[at]) at++
      failure(obj, scope, row, 'mismatch', `byte ${at} differs (lengths ${full.length} -> ${reser.length})`)
      continue
    }
    ok++
  }
  process.stderr.write(`check: ok=${ok} fail=${fail} total=${ok + fail} reasons=${JSON.stringify(reasons)}\n`)
  if (fail > 0) process.exitCode = 1
}

function main () {
  const { mode, opts } = parseArgs(process.argv)
  if (!opts.protocol) {
    process.stderr.write('missing --protocol <protocol.json>\n')
    usage(2)
  }
  let protocolJson
  try {
    protocolJson = protoLib.loadProtocolJson(opts.protocol)
    if (opts.patches) protoLib.applyPatches(protocolJson, opts.patches)
  } catch (e) {
    process.stderr.write(`cannot load protocol: ${e.message}\n`)
    process.exit(2)
  }
  if (mode === 'list') {
    cmdList(opts.protocol, protocolJson)
  } else if (mode === 'gen') {
    cmdGen(opts.protocol, protocolJson, opts)
  } else if (mode === 'check') {
    cmdCheck(opts.protocol, protocolJson, opts).catch((e) => {
      process.stderr.write(`check failed: ${e.stack || e}\n`)
      process.exit(2)
    })
  } else {
    process.stderr.write(`unknown mode: ${mode}\n`)
    usage(2)
  }
}

main()
