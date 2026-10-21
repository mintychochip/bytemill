// Shared protocol loading / compilation, mirroring node-minecraft-protocol.
//
// Setup per (state, direction), exactly like node-minecraft-protocol:
//   const compiler = new ProtoDefCompiler()
//   compiler.addTypes(require('./compiler-minecraft'))
//   compiler.addProtocol(protocolJson, [state, direction])
//   nbt.addTypesToCompiler('big', compiler)
//   const proto = compiler.compileProtoDefSync()
// A packet buffer is `varint id + payload` rooted at type 'packet':
//   proto.createPacketBuffer('packet', { name, params })
//   proto.parsePacketBuffer('packet', buf)
'use strict'

const fs = require('fs')
const { ProtoDefCompiler } = require('protodef').Compiler
const nbt = require('prismarine-nbt')
const minecraftTypes = require('../vendor/compiler-minecraft')

const STATES = ['handshaking', 'status', 'login', 'configuration', 'play']
const DIRECTIONS = ['toClient', 'toServer']

// Cache of compiled scopes inside one process: key `${file}\0${state}\0${direction}`.
const scopeCache = new Map()
function loadProtocolJson (file) {
  return JSON.parse(fs.readFileSync(file, 'utf8'))
}

// Apply a BML patch file (schemas/versions/patches/*.json) to a loaded protocol: each patch
// replaces or adds named types in `global` or in one `<state>.<toClient|toServer>` scope, exactly
// as `bml import-versions` does, so the oracle compiles the same description BML imported.
function applyPatches (protocolJson, patchFile) {
  const { patches } = JSON.parse(fs.readFileSync(patchFile, 'utf8'))
  for (const patch of patches) {
    let table
    if (patch.scope === 'global') {
      table = protocolJson.types
    } else {
      const [state, direction] = patch.scope.split('.')
      table = protocolJson[state][direction].types
    }
    for (const [name, def] of Object.entries(patch.types)) table[name] = def
  }
  return protocolJson
}

// Introspected type table of one compiled scope. `types` maps a type name to
// either the string 'native' (implemented by a native/context function, or a
// parametrizable constructor used bare) or its definition (a string alias or
// a [constructor, args] expression). `param` holds parametrizable constructor
// names, `context` the nbt context natives ('compound', 'nbtTagName').
function compileScope (protocolJson, state, direction) {
  const compiler = new ProtoDefCompiler()
  compiler.addTypes(minecraftTypes)
  compiler.addProtocol(protocolJson, [state, direction])
  nbt.addTypesToCompiler('big', compiler)
  // First-byte probes: entityMetadataLoop items must not start with endVal,
  // topBitSetTerminatedArray items must start with a byte < 128 (the reader
  // clears bit 7 in place, which corrupts parsing when the writer did not
  // set it). Each loop occurrence's element expression is registered as a
  // synthetic type so the generator can serialise single elements and check
  // their first byte. Occurrences using '../' cannot compile at root and
  // are skipped (packet-level retry remains the fallback).
  const probeList = collectLoopProbes(protocolJson, state, direction)
  const probeTypes = {}
  probeList.forEach((p, i) => { probeTypes[`__oracle_probe_${i}`] = p.expr })
  compiler.addTypesToCompile(probeTypes)
  const proto = compiler.compileProtoDefSync()
  const rc = compiler.readCompiler
  const probeMap = new Map()
  probeList.forEach((p, i) => {
    probeMap.set(p.args, { name: `__oracle_probe_${i}`, kind: p.kind, endVal: p.endVal })
  })
  return {
    proto,
    probeMap,
    table: {
      types: Object.assign({}, rc.types),
      param: new Set(Object.keys(rc.parameterizableTypes)),
      context: new Set(Object.keys(rc.context))
    }
  }
}

// Every entityMetadataLoop / topBitSetTerminatedArray occurrence in the
// scope's type environment: { kind, endVal, args, expr } where `args` is the
// loop's own args object (identity key for the generator) and `expr` is the
// element type expression to register as a probe.
function collectLoopProbes (protocolJson, state, direction) {
  const out = []
  const dicts = []
  if (protocolJson.types) dicts.push(protocolJson.types)
  const scope = protocolJson[state] && protocolJson[state][direction]
  if (scope && scope.types) dicts.push(scope.types)
  function walk (t) {
    if (typeof t === 'string' || !Array.isArray(t) || t.length === 0) return
    const ctor = t[0]
    const args = t[1]
    if (ctor === 'entityMetadataLoop' || ctor === 'topBitSetTerminatedArray') {
      if (args && args.type && !JSON.stringify(args).includes('"../')) {
        out.push({ kind: ctor, endVal: args.endVal, args, expr: args.type })
      }
    }
    if (ctor === 'container' && Array.isArray(args)) {
      for (const f of args) walk(f.type)
    } else if (ctor === 'array') {
      walk(args.type)
    } else if (ctor === 'switch') {
      for (const k of Object.keys(args.fields || {})) walk(args.fields[k])
      if (args.default) walk(args.default)
    } else if (ctor === 'option' || ctor === 'count') {
      walk(args && args.type !== undefined ? args.type : args)
    } else if (ctor === 'entityMetadataLoop' || ctor === 'topBitSetTerminatedArray') {
      if (args && args.type) walk(args.type)
    } else if (ctor === 'registryEntryHolder') {
      if (args && args.otherwise) walk(args.otherwise.type)
    } else if (ctor === 'registryEntryHolderSet') {
      if (args && args.base) walk(args.base.type)
      if (args && args.otherwise) walk(args.otherwise.type)
    }
  }
  for (const dict of dicts) {
    for (const k of Object.keys(dict)) {
      if (typeof dict[k] !== 'string') walk(dict[k])
    }
  }
  return out
}

function getScope (file, protocolJson, state, direction) {
  const key = `${file}\0${state}\0${direction}`
  let scope = scopeCache.get(key)
  if (!scope) {
    scope = compileScope(protocolJson, state, direction)
    scope.file = file
    scope.state = state
    scope.direction = direction
    scopeCache.set(key, scope)
  }
  return scope
}

function hasPacketType (protocolJson, state, direction) {
  const scope = protocolJson[state] && protocolJson[state][direction]
  return !!(scope && scope.types && scope.types.packet)
}

// The `packet` container of one scope: fields `name` (a mapper
// varint/id -> name) and `params` (a switch on `name` -> packet body type).
// Returns { mappings, fields } or throws if the shape is unexpected.
function packetShape (scope) {
  const def = scope.table.types.packet
  if (!Array.isArray(def) || def[0] !== 'container') {
    throw new Error(`scope ${scope.state}.${scope.direction}: 'packet' is not a container`)
  }
  const fields = def[1]
  const nameField = fields.find((f) => f.name === 'name')
  const paramsField = fields.find((f) => f.name === 'params')
  if (!nameField || !Array.isArray(nameField.type) || nameField.type[0] !== 'mapper') {
    throw new Error(`scope ${scope.state}.${scope.direction}: 'packet.name' is not a mapper`)
  }
  if (!paramsField || !Array.isArray(paramsField.type) || paramsField.type[0] !== 'switch') {
    throw new Error(`scope ${scope.state}.${scope.direction}: 'packet.params' is not a switch`)
  }
  return { mappings: nameField.type[1].mappings, fields: paramsField.type[1].fields }
}

function hexKeyToId (key) {
  return /^0x[0-9a-f]+$/i.test(key) ? parseInt(key.slice(2), 16) : parseInt(key, 10)
}

// Every packet declared in the scope's packet mapper, sorted by id.
function packetTable (scope) {
  const { mappings, fields } = packetShape(scope)
  const rows = Object.keys(mappings).map((key) => ({
    id: hexKeyToId(key),
    name: mappings[key],
    arm: Object.prototype.hasOwnProperty.call(fields, mappings[key]) ? fields[mappings[key]] : undefined
  }))
  rows.sort((a, b) => a.id - b.id)
  return rows
}

function scopesOf (protocolJson) {
  const out = []
  for (const state of STATES) {
    if (!protocolJson[state]) continue
    for (const direction of DIRECTIONS) {
      if (protocolJson[state][direction]) out.push([state, direction])
    }
  }
  return out
}

module.exports = {
  applyPatches,
  STATES,
  DIRECTIONS,
  loadProtocolJson,
  compileScope,
  getScope,
  hasPacketType,
  packetShape,
  packetTable,
  hexKeyToId,
  scopesOf
}
