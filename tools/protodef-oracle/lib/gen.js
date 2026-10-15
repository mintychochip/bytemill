// Type-directed random packet generator.
//
// The generator walks the ProtoDef type tree (global + scope-local type
// table, see lib/proto.js) and builds a value for every field, mirroring the
// semantics of the compiled node-minecraft-protocol codec:
//
// - containers are generated in order with a scope holding every previously
//   generated field, so `switch`/`mapper`/count references resolve exactly as
//   the compiled code would see them (lexical scopes for nested containers,
//   `../x` for the parent scope, `a/b` for member access, `$param` for
//   parametrised user types such as particleData / entityMetadataItem, whose
//   arguments are positional field references like the compiled call sites).
// - `mapper` produces the mapped NAME in read form (numeric-looking names as
//   numbers, 'true'/'false' as booleans, like sanitizeMappings); with ~30% it
//   produces an unmapped raw value so `switch` default arms get exercised.
// - `switch` selects its arm with JS strict-equality semantics (numeric keys
//   match numbers, 'true'/'false' match booleans, other keys match strings).
// - `anon: true` container/switch/bitfield fields flatten into the parent.
// - arrays with a `count` naming a field reuse/overwrite that field so the
//   two stay consistent; `buffer{count: n}` yields exactly n bytes;
//   countType arrays yield 0..3 elements.
// - floats are finite (f32 pre-rounded with Math.fround), strings mix ASCII,
//   BMP and astral characters (never lone surrogates), varints cover int32
//   edges, varlong/i64/u64 cover the full 64-bit range (BigInt values).
// - NBT (compound/list) recursion is bounded by maxNbtDepth.
//
// Deterministic: every draw comes from one mulberry32 stream seeded by --seed.
'use strict'

const { hexKeyToId } = require('./proto')

function mulberry32 (seed) {
  let a = seed >>> 0
  return function () {
    a |= 0; a = (a + 0x6D2B79F5) | 0
    let t = Math.imul(a ^ (a >>> 15), 1 | a)
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}

function hex2dec (num) {
  if (/^0x[0-9a-f]+$/i.test(num)) return parseInt(num.slice(2), 16)
  return num
}

// Mirror of protodef compiler-utils sanitizeMappings: mapped values that look
// numeric become numbers, 'true'/'false' become booleans.
function sanitizeMappings (json) {
  const ret = {}
  for (const key in json) {
    let val = json[key]
    if (!isNaN(val)) val = Number(val)
    if (val === 'true') val = true
    if (val === 'false') val = false
    ret[hex2dec(key)] = val
  }
  return ret
}

// Lexical scope chain. `vars` holds generated field values of one container
// level; `dollars` is the current parametrised-type call frame ($params),
// shared by reference with nested container levels.
class Scope {
  constructor (parent = null, dollars = null) {
    this.parent = parent
    this.vars = Object.create(null)
    this.dollars = dollars || (parent ? parent.dollars : null)
  }
}

// Resolve 'x', '../x', 'a/b' like Compiler.getField (value side).
function resolvePath (scope, path) {
  const parts = path.split('/')
  let cur = scope
  let i = 0
  while (i < parts.length && parts[i] === '..') {
    cur = cur ? cur.parent : null
    i++
  }
  if (!cur) return undefined
  let found = null
  for (let s = cur; s; s = s.parent) {
    if (Object.prototype.hasOwnProperty.call(s.vars, parts[i])) {
      found = s
      break
    }
  }
  if (!found) return undefined
  let val = found.vars[parts[i]]
  i++
  for (; i < parts.length; i++) {
    if (val === null || val === undefined) return undefined
    val = val[parts[i]]
  }
  return val
}

// Which arm a compiled `switch(compare)` would take, under strict equality:
// numeric-looking keys match numbers, 'true'/'false' match booleans, every
// other key matches strings only.
function strictArmKey (fields, cmp) {
  for (const k of Object.keys(fields)) {
    if (k === 'true' || k === 'false') {
      if (typeof cmp === 'boolean' && String(cmp) === k) return k
    } else if (k !== '' && !isNaN(k)) {
      if (typeof cmp === 'number' && Number(k) === cmp) return k
    } else {
      if (typeof cmp === 'string' && cmp === k) return k
    }
  }
  return undefined
}

const NBT_TAGS = ['byte', 'short', 'int', 'long', 'float', 'double',
  'string', 'byteArray', 'intArray', 'longArray', 'list', 'compound']
const NBT_SCALARS = NBT_TAGS.slice(0, 10)

class Generator {
  constructor (table, seed, options = {}) {
    this.table = table
    this.rand = mulberry32(seed >>> 0)
    this.maxNbtDepth = options.depth == null ? 3 : options.depth
    this.nesting = 0
    // First-byte probes (see lib/proto.js): set per scope by the CLI.
    this.probeProto = null
    this.probeMap = null
  }
  rint (a, b) {
    return a + Math.floor(this.rand() * (b - a + 1))
  }

  pick (arr) {
    return arr[Math.floor(this.rand() * arr.length)]
  }

  maybe (p) {
    return this.rand() < p
  }

  uniform (a, b) {
    return a + this.rand() * (b - a)
  }

  // ---- leaves ----

  genVarint () {
    const r = this.rand()
    if (r < 0.15) return this.pick([-2147483648, -1, 0, 1, 2147483647, 255, 32767, -32768])
    return Math.floor(this.uniform(0, 4294967296)) - 2147483648
  }

  genVarlong () {
    const r = this.rand()
    if (r < 0.2) {
      return this.pick([-(1n << 63n), (1n << 63n) - 1n, -1n, 0n, 1n,
        -2147483648n, 2147483647n, -4294967296n, 4294967295n])
    }
    const hi = BigInt(Math.floor(this.uniform(0, 4294967296)))
    const lo = BigInt(Math.floor(this.uniform(0, 4294967296)))
    let v = (hi << 32n) | lo
    if (v >= (1n << 63n)) v -= (1n << 64n)
    return v
  }

  genU64 () {
    const hi = BigInt(Math.floor(this.uniform(0, 4294967296)))
    const lo = BigInt(Math.floor(this.uniform(0, 4294967296)))
    return (hi << 32n) | lo
  }

  genI64 () {
    const v = this.genU64()
    return v >= (1n << 63n) ? v - (1n << 64n) : v
  }

  randFinite (edges) {
    const r = this.rand()
    if (r < 0.06) return 0
    if (r < 0.10) return this.pick(edges)
    const mag = this.rint(-12, 12)
    return (this.rand() * 2 - 1) * Math.pow(10, mag)
  }

  genF32 () {
    return Math.fround(this.randFinite([1, -1, 0.5, 3.4028235e38, -3.4028235e38, 1.1754944e-38]))
  }

  genF64 () {
    return this.randFinite([1, -1, 0.5, Number.MAX_VALUE, -Number.MAX_VALUE, Number.MIN_VALUE])
  }

  genInt (bits, signed) {
    if (signed) return this.rint(-(2 ** (bits - 1)), 2 ** (bits - 1) - 1)
    return this.rint(0, 2 ** bits - 1)
  }

  genUuid () {
    const h = []
    for (let i = 0; i < 16; i++) h.push(this.rint(0, 255).toString(16).padStart(2, '0'))
    const s = h.join('')
    return `${s.slice(0, 8)}-${s.slice(8, 12)}-${s.slice(12, 16)}-${s.slice(16, 20)}-${s.slice(20)}`
  }

  genBytes (n) {
    const b = Buffer.allocUnsafe(n)
    for (let i = 0; i < n; i++) b[i] = this.rint(0, 255)
    return b
  }

  // ASCII plus some BMP and astral characters, never lone surrogates, never NUL.
  genString (len) {
    if (len === undefined) len = this.rint(0, 24)
    let s = ''
    for (let i = 0; i < len; i++) {
      const r = this.rand()
      if (r < 0.7) {
        s += String.fromCodePoint(this.rint(0x20, 0x7e))
      } else if (r < 0.9) {
        let cp
        do {
          cp = this.rint(0xa0, 0xffff)
        } while (cp >= 0xd800 && cp <= 0xdfff)
        s += String.fromCodePoint(cp)
      } else {
        s += String.fromCodePoint(this.rint(0x10000, 0x10ffff))
      }
    }
    return s
  }

  genTagName () {
    const alpha = 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_'
    const n = this.rint(1, 12)
    let s = ''
    for (let i = 0; i < n; i++) s += alpha[this.rint(0, alpha.length - 1)]
    return s
  }

  // entityDelta (vanilla 26.3 VecDelta): stepCount = properties >>> 1 must
  // agree with the payload form. Both forms are produced: stepCount 0
  // (linear triple i16) and 1..3 stepped entries.
  genEntityDelta () {
    const i16 = () => this.genInt(16, true)
    if (this.maybe(0.5)) {
      return { properties: this.rint(0, 1), linear: { x: i16(), y: i16(), z: i16() } }
    }
    const k = this.rint(1, 3)
    const steps = []
    for (let i = 0; i < k; i++) {
      steps.push({ ticks: this.genVarint(), x: i16(), y: i16(), z: i16() })
    }
    return { properties: (k << 1) | this.rint(0, 1), steps }
  }

  genLpVec3 () {
    if (this.maybe(0.15)) return { x: 0, y: 0, z: 0 }
    const m = Math.pow(10, this.rint(-4, 9))
    return { x: (this.rand() * 2 - 1) * m, y: (this.rand() * 2 - 1) * m, z: (this.rand() * 2 - 1) * m }
  }

  genNative (name) {
    switch (name) {
      case 'varint': return this.genVarint()
      case 'varlong': return this.genVarlong()
      case 'varint64': return this.genU64()
      case 'varint128': return this.genU64()
      case 'zigzag32': return this.genVarint()
      case 'zigzag64': return this.genI64()
      case 'bool': return this.maybe(0.5)
      case 'i8': return this.genInt(8, true)
      case 'u8': return this.genInt(8, false)
      case 'i16': case 'li16': return this.genInt(16, true)
      case 'u16': case 'lu16': return this.genInt(16, false)
      case 'i32': case 'li32': return this.genVarint()
      case 'u32': case 'lu32': return Math.floor(this.uniform(0, 4294967296))
      case 'f32': case 'lf32': return this.genF32()
      case 'f64': case 'lf64': return this.genF64()
      case 'i64': case 'li64': return this.genI64()
      case 'u64': case 'lu64': return this.genU64()
      case 'UUID': return this.genUuid()
      case 'restBuffer': return this.genBytes(this.rint(0, 16))
      case 'entityDelta': return this.genEntityDelta()
      case 'lpVec3': return this.genLpVec3()
      case 'void': return undefined
      case 'cstring': return this.genString(this.rint(0, 24))
      case 'compressedNbt':
        if (this.maybe(0.3)) return undefined
        return this.genFullTag(0)
      default: throw new Error(`no generator for native type '${name}'`)
    }
  }

  // ---- NBT (depth-bounded) ----

  pickTag (depth) {
    const pool = depth < this.maxNbtDepth ? NBT_TAGS : NBT_SCALARS
    return this.pick(pool)
  }

  genNbtPayload (tag, depth) {
    switch (tag) {
      case 'end': return undefined
      case 'byte': return this.genInt(8, true)
      case 'short': return this.genInt(16, true)
      case 'int': return this.genVarint()
      case 'long': return this.genI64()
      case 'float': return this.genF32()
      case 'double': return this.genF64()
      case 'string': return this.genString(this.rint(0, 40))
      case 'byteArray': {
        const n = this.rint(0, 8)
        const out = []
        for (let i = 0; i < n; i++) out.push(this.genInt(8, true))
        return out
      }
      case 'intArray': {
        const n = this.rint(0, 6)
        const out = []
        for (let i = 0; i < n; i++) out.push(this.genVarint())
        return out
      }
      case 'longArray': {
        const n = this.rint(0, 6)
        const out = []
        for (let i = 0; i < n; i++) out.push(this.genI64())
        return out
      }
      case 'list': {
        const sub = this.pickTag(depth)
        const n = this.rint(0, 2)
        const value = []
        for (let i = 0; i < n; i++) value.push(this.genNbtPayload(sub, depth + 1))
        return { type: sub, value }
      }
      case 'compound': return this.genCompound(depth)
      default: throw new Error(`no generator for NBT tag '${tag}'`)
    }
  }

  // The `compound` context native: { name: { type, value } }, empty at the depth limit.
  genCompound (depth) {
    if (depth >= this.maxNbtDepth) return {}
    const out = Object.create(null)
    const n = this.rint(0, 3)
    for (let i = 0; i < n; i++) {
      const tag = this.pickTag(depth)
      out[this.genTagName()] = { type: tag, value: this.genNbtPayload(tag, depth + 1) }
    }
    return out
  }
  // The `list` type, mirroring its definition but with valid, depth-capped tags.
  genList (depth) {
    const tag = this.pickTag(depth)
    const n = this.rint(0, 2)
    const value = []
    for (let i = 0; i < n; i++) value.push(this.genNbtPayload(tag, depth + 1))
    return { type: tag, value }
  }

  genFullTag (depth) {
    const tag = this.pickTag(depth)
    return { type: tag, name: this.genTagName(), value: this.genNbtPayload(tag, depth + 1) }
  }

  genAnonTag (depth) {
    const tag = this.pickTag(depth)
    return { type: tag, value: this.genNbtPayload(tag, depth + 1) }
  }

  // ---- dispatch ----

  genType (t, scope, nbtDepth = 0) {
    if (typeof t === 'string') return this.genNamed(t, scope, nbtDepth)
    const ctor = t[0]
    const args = t[1]
    switch (ctor) {
      case 'container': return this.genContainer(args, scope, nbtDepth)
      case 'array': return this.genArray(args, scope, nbtDepth)
      case 'switch': return this.genSwitch(args, scope, nbtDepth)
      case 'option': return this.genOption(args, scope, nbtDepth)
      case 'mapper': return this.genMapper(args, scope, nbtDepth)
      case 'pstring': return this.genPstring(args, scope)
      case 'buffer': return this.genBuffer(args, scope)
      case 'bitfield': return this.genBitfield(args)
      case 'bitflags': return this.genBitflags(args)
      case 'void': return undefined
      case 'cstring': return this.genString(this.rint(0, 24))
      case 'count': return this.genType(args.type, scope, nbtDepth)
      case 'entityMetadataLoop': return this.genEntityMetadataLoop(args, scope, nbtDepth)
      case 'topBitSetTerminatedArray': return this.genTopBitSet(args, scope, nbtDepth)
      case 'registryEntryHolder': return this.genRegistryEntryHolder(args, scope, nbtDepth)
      case 'registryEntryHolderSet': return this.genRegistryEntryHolderSet(args, scope, nbtDepth)
      case 'optionalNbtType': return this.genOption(args.tagType, scope, nbtDepth)
      default: return this.genUserType(ctor, args, scope, nbtDepth)
    }
  }

  genContainer (fields, scope, nbtDepth) {
    this.nesting++
    try {
      const child = new Scope(scope)
      this.genFieldsInto(fields, child, nbtDepth)
      return child.vars
    } finally {
      this.nesting--
    }
  }

  // Generate fields into `scope.vars`. Anon containers reuse the same scope
  // object (they are inlined one level by containerInlining).
  genFieldsInto (fields, scope, nbtDepth) {
    const result = scope.vars
    const constrained = new Set()
    // Fields targeted by an array/pstring/buffer `count` in this container:
    // keep them small so packets stay small, and consistent (see countLen).
    // Fields read by a same-container `switch` compareTo: bias toward arm
    // keys so arms get exercised (see genBiasedField).
    for (const f of fields) {
      if (!f.anon && Array.isArray(f.type) &&
        (f.type[0] === 'array' || f.type[0] === 'pstring' || f.type[0] === 'buffer') &&
        typeof f.type[1].count === 'string') {
        constrained.add(f.type[1].count)
      }
    }
    // Fields read by a same-container `switch` compareTo: bias toward arm
    // keys so arms get exercised (mapper-typed fields are skipped in
    // genBiasedField: genMapper already balances mapped vs raw values).
    const armBias = new Map()
    for (const f of fields) {
      const st = f.type
      if (Array.isArray(st) && st[0] === 'switch' && typeof st[1].compareTo === 'string') {
        const ct = st[1].compareTo
        if (ct.startsWith('$') || ct.includes('/')) continue
        if (!armBias.has(ct)) armBias.set(ct, [])
        armBias.get(ct).push(...Object.keys(st[1].fields || {}))
      }
    }
    for (const f of fields) {
      const t = f.type
      if (f.anon && Array.isArray(t) && t[0] === 'container') {
        // Inlined one level into the parent scope, like containerInlining.
        this.genFieldsInto(t[1], scope, nbtDepth)
      } else if (f.anon && Array.isArray(t) && t[0] === 'bitfield') {
        Object.assign(result, this.genBitfield(t[1]))
      } else if (f.anon && Array.isArray(t) && t[0] === 'switch') {
        // Spread into the parent value, like `_shouldBeInlined`.
        const v = this.genSwitch(t[1], scope, nbtDepth)
        if (v !== undefined) Object.assign(result, v)
      } else if (Object.prototype.hasOwnProperty.call(result, f.name)) {
        // Pre-bound (e.g. the packet `name` field).
      } else {
        let v = this.genBiasedField(f.name, t, armBias.get(f.name), scope, nbtDepth)
        if (constrained.has(f.name) && typeof v === 'number') v = this.rint(0, 3)
        result[f.name] = v
      }
    }
  }

  // Scalar kind of a field type for arm-key biasing, or null (e.g. mappers,
  // containers: generated unbiased).
  biasKind (t, depth = 0) {
    if (depth > 10) return null
    if (Array.isArray(t)) {
      if (t[0] === 'pstring' || t[0] === 'cstring') return 'string'
      return null
    }
    if (t === 'bool') return 'boolean'
    if (['varint', 'u8', 'i8', 'u16', 'i16', 'i32', 'u32', 'li16', 'lu16', 'li32', 'lu32'].includes(t)) return 'number'
    const def = this.table.types[t]
    if (def === undefined || def === 'native') return null
    if (typeof def === 'string') return this.biasKind(def, depth + 1)
    if (def[0] === 'pstring') return 'string'
    return null
  }

  genBiasedField (name, t, armKeys, scope, nbtDepth) {
    if (armKeys && armKeys.length > 0 && this.maybe(0.6)) {
      const kind = this.biasKind(t)
      if (kind) {
        const cands = armKeys.filter((k) => {
          if (kind === 'boolean') return k === 'true' || k === 'false'
          if (kind === 'number') return k !== '' && !isNaN(k)
          return k !== 'true' && k !== 'false' && (k === '' || isNaN(k))
        })
        if (cands.length > 0) {
          const key = this.pick(cands)
          if (kind === 'boolean') return key === 'true'
          if (kind === 'number') return Number(key)
          return key
        }
      }
    }
    return this.genType(t, scope, nbtDepth)
  }

  genNamed (name, scope, nbtDepth, aliasDepth = 0) {
    // NBT roots handled directly so tags stay valid (see genCompound/genList).
    if (name === 'compound') return this.genCompound(nbtDepth)
    if (name === 'nbtTagName') return this.genTagName()
    if (name === 'list') return this.genList(nbtDepth)
    if (name === 'nbt') return this.genFullTag(nbtDepth)
    if (name === 'anonymousNbt') return this.genAnonTag(nbtDepth)
    if (this.table.context.has(name)) throw new Error(`no generator for context type '${name}'`)
    const def = this.table.types[name]
    if (def === undefined) throw new Error(`unknown type '${name}'`)
    if (def === 'native') return this.genNative(name)
    if (typeof def === 'string') {
      if (aliasDepth > 10) throw new Error(`alias cycle at '${name}'`)
      return this.genNamed(def, scope, nbtDepth, aliasDepth + 1)
    }
    return this.genType(def, scope, nbtDepth)
  }

  // Parametrised user type (e.g. ["particleData", {"compareTo": "particleId"}]):
  // arguments are positional field references evaluated at the call site and
  // visible in the body as $params, like the compiled callType.
  genUserType (ctor, args, scope, nbtDepth) {
    const def = this.table.types[ctor]
    if (def === undefined) throw new Error(`unknown type '${ctor}'`)
    if (def === 'native') return this.genNative(ctor)
    if (typeof def === 'string') return this.genNamed(def, scope, nbtDepth)
    const dollars = Object.create(null)
    if (args) {
      for (const k of Object.keys(args)) dollars['$' + k] = this.evalArg(args[k], scope)
    }
    return this.genType(def, new Scope(scope, dollars), nbtDepth)
  }

  evalArg (v, scope) {
    if (typeof v !== 'string') return v
    if (v.startsWith('$')) return scope.dollars ? scope.dollars[v] : undefined
    const r = resolvePath(scope, v)
    return r === undefined ? v : r
  }


  // `count` naming a field: use the field's value (JS `i < count` semantics),
  // overwriting it with a small length when it is a plain same-container
  // number field so the two stay consistent.
  countLen (scope, ref) {
    const v = resolvePath(scope, ref)
    if (typeof v === 'number') {
      if (Number.isInteger(v) && v >= 0 && v <= 3) return v
      const len = this.rint(0, 3)
      if (ref.indexOf('/') === -1 && Object.prototype.hasOwnProperty.call(scope.vars, ref)) {
        scope.vars[ref] = len
      }
      return len
    }
    if (typeof v === 'bigint') {
      const n = Number(v)
      if (Number.isInteger(n) && n >= 0 && n <= 3) return n
      return this.rint(0, 3)
    }
    let n = Number(v)
    if (!isFinite(n)) n = 0
    return Math.max(0, Math.min(64, Math.floor(n)))
  }

  genArray (arr, scope, nbtDepth) {
    let len
    if (arr.countType) {
      len = this.rint(0, 3)
    } else if (arr.count === undefined) {
      throw new Error('array without count')
    } else if (typeof arr.count === 'number' || /^\d+$/.test(String(arr.count))) {
      len = parseInt(arr.count, 10)
    } else {
      len = this.countLen(scope, arr.count)
    }
    const out = []
    for (let i = 0; i < len; i++) out.push(this.genType(arr.type, scope, nbtDepth))
    return out
  }

  genSwitch (sw, scope, nbtDepth) {
    let cmp
    if (sw.compareToValue !== undefined) {
      cmp = sw.compareToValue
    } else if (sw.compareTo === undefined) {
      cmp = undefined
    } else if (sw.compareTo.startsWith('$')) {
      cmp = scope.dollars ? scope.dollars[sw.compareTo] : undefined
    } else {
      cmp = resolvePath(scope, sw.compareTo)
    }
    const key = strictArmKey(sw.fields, cmp)
    const arm = key !== undefined ? sw.fields[key] : sw.default
    if (arm === undefined) return undefined
    return this.genType(arm, scope, nbtDepth)
  }

  genOption (inner, scope, nbtDepth) {
    if (this.maybe(0.25)) return undefined
    return this.genType(inner, scope, nbtDepth)
  }

  genMapper (mapper, scope, nbtDepth) {
    const sanitized = sanitizeMappings(mapper.mappings)
    const keys = Object.keys(sanitized)
    if (this.maybe(0.3)) {
      for (let i = 0; i < 20; i++) {
        const raw = this.genType(mapper.type, scope, nbtDepth)
        if (!Object.prototype.hasOwnProperty.call(sanitized, String(raw))) return raw
      }
    }
    return sanitized[this.pick(keys)]
  }

  genPstring (str, scope) {
    if (str.countType) return this.genString()
    if (typeof str.count === 'number') {
      // Fixed byte count: ASCII is one byte per char.
      let s = ''
      for (let i = 0; i < str.count; i++) s += String.fromCodePoint(this.rint(0x20, 0x7e))
      return s
    }
    if (typeof str.count === 'string') {
      const len = this.countLen(scope, str.count)
      let s = ''
      for (let i = 0; i < len; i++) s += String.fromCodePoint(this.rint(0x20, 0x7e))
      return s
    }
    throw new Error('pstring without count')
  }

  genBuffer (buf, scope) {
    if (buf.countType) return this.genBytes(this.rint(0, 16))
    if (typeof buf.count === 'number') return this.genBytes(buf.count)
    if (typeof buf.count === 'string') return this.genBytes(this.countLen(scope, buf.count))
    throw new Error('buffer without count')
  }

  genBitfield (members) {
    const out = Object.create(null)
    for (const { name, size, signed } of members) {
      out[name] = signed ? this.genInt(size, true) : this.genInt(size, false)
    }
    return out
  }

  genBitflags (flags) {
    const names = Array.isArray(flags.flags) ? flags.flags : Object.keys(flags.flags)
    const value = { _value: flags.big ? 0n : 0 }
    for (const name of names) value[name] = this.maybe(0.5)
    return value
  }

  // Serialise one loop element through its probe type and check the first
  // byte: entityMetadataLoop items must not start with endVal (the reader
  // would stop early), topBitSetTerminatedArray items must start < 128
  // (the reader clears bit 7 in place). Without a probe (or when the probe
  // cannot run, e.g. $refs needing an outer frame) accept the value and let
  // the packet-level self-check decide.
  firstByteOk (loopArgs, value) {
    if (!this.probeProto || !this.probeMap) return true
    const probe = this.probeMap.get(loopArgs)
    if (!probe) return true
    try {
      const buf = this.probeProto.createPacketBuffer(probe.name, value)
      if (buf.length === 0) return true
      if (probe.kind === 'entityMetadataLoop') return buf[0] !== probe.endVal
      return buf[0] < 128
    } catch (e) {
      return true
    }
  }

  genLoopItem (loopArgs, elementType, scope, nbtDepth) {
    let v = this.genType(elementType, scope, nbtDepth)
    for (let t = 0; t < 10 && !this.firstByteOk(loopArgs, v); t++) {
      v = this.genType(elementType, scope, nbtDepth)
    }
    return v
  }

  genEntityMetadataLoop (loop, scope, nbtDepth) {
    const len = this.rint(0, 3)
    const out = []
    for (let i = 0; i < len; i++) out.push(this.genLoopItem(loop, loop.type, scope, nbtDepth))
    return out
  }

  // Needs >= 1 element (an empty value would have no last byte to clear bit 7).
  genTopBitSet (arr, scope, nbtDepth) {
    const len = this.rint(1, 3)
    const out = []
    for (let i = 0; i < len; i++) out.push(this.genLoopItem(arr, arr.type, scope, nbtDepth))
    return out
  }

  genRegistryEntryHolder (holder, scope, nbtDepth) {
    if (this.maybe(0.5)) return { [holder.baseName]: this.rint(0, 5) }
    return { [holder.otherwise.name]: this.genType(holder.otherwise.type, scope, nbtDepth) }
  }

  genRegistryEntryHolderSet (holder, scope, nbtDepth) {
    if (this.maybe(0.5)) return { [holder.base.name]: this.genType(holder.base.type, scope, nbtDepth) }
    const n = this.rint(0, 2)
    const ids = []
    for (let i = 0; i < n; i++) ids.push(this.genType(holder.otherwise.type, scope, nbtDepth))
    return { [holder.otherwise.name]: ids }
  }
}

// Build the root value for one declared packet: the `name` field is pre-bound
// so the `params` switch selects the packet's own arm.
function buildPacketValue (gen, arm, packetName) {
  const scope = new Scope(null)
  scope.vars.name = packetName
  const params = arm === undefined ? undefined : gen.genType(arm, scope, 0)
  return { name: packetName, params }
}

module.exports = {
  Generator,
  Scope,
  resolvePath,
  sanitizeMappings,
  strictArmKey,
  buildPacketValue,
  hexKeyToId
}
