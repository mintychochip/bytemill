// Vendored from node-minecraft-protocol (MIT licence) master commit 1c8318927,
// file src/minecraft.js, with TWO deliberate deviations (see README.md):
// (1) the varlong read/write/sizeOf below implement Mojang's VarLong (unsigned base-128 groups
// of the 64-bit two's-complement pattern, so a negative long takes ten bytes; BigInt values).
// Upstream implements varlong with readVarInt/writeVarInt/sizeOfVarInt (32-bit), which
// corrupts values outside the int32 range.
// (2) a NEW entityDelta native (read/write/sizeOf at the end of this file),
// absent upstream, per vanilla 26.3 VecDelta.java + ClientboundMoveEntityPacket.java.
'use strict'

const nbt = require('prismarine-nbt')
const UUID = require('uuid-1345')
const zlib = require('zlib')
const [readVarInt, writeVarInt, sizeOfVarInt] = require('protodef').types.varint
const [readLpVec3, writeLpVec3, sizeOfLpVec3] = require('./lpVec3')

module.exports = {
  varlong: [readVarLong, writeVarLong, sizeOfVarLong],
  UUID: [readUUID, writeUUID, 16],
  compressedNbt: [readCompressedNbt, writeCompressedNbt, sizeOfCompressedNbt],
  restBuffer: [readRestBuffer, writeRestBuffer, sizeOfRestBuffer],
  entityMetadataLoop: [readEntityMetadata, writeEntityMetadata, sizeOfEntityMetadata],
  topBitSetTerminatedArray: [readTopBitSetTerminatedArray, writeTopBitSetTerminatedArray, sizeOfTopBitSetTerminatedArray],
  entityDelta: [readEntityDelta, writeEntityDelta, sizeOfEntityDelta],
  lpVec3: [readLpVec3, writeLpVec3, sizeOfLpVec3]
}
const PartialReadError = require('protodef').utils.PartialReadError

// DELIBERATE DEVIATION FROM UPSTREAM (see header + README.md): upstream
// implements these three functions with the 32-bit readVarInt/writeVarInt/
// sizeOfVarInt. Below is a correct 64-bit *signed* LEB128 (two's complement,
// up to 10 bytes). Values are BigInts in [-2^63, 2^63 - 1].
function readVarLong (buffer, offset) {
  // Mojang's FriendlyByteBuf.readVarLong: unsigned base-128 groups, lowest first, at most ten
  // bytes; the last group only supplies bit 63, and the result is the 64-bit two's-complement
  // pattern read as signed.
  let result = 0n
  let shift = 0n
  let cursor = offset
  while (true) {
    if (cursor >= buffer.length) { throw new PartialReadError('Unexpected buffer end while reading VarLong') }
    const byte = buffer.readUInt8(cursor)
    result |= (BigInt(byte) & 0x7Fn) << shift
    cursor++
    if (!(byte & 0x80)) {
      return { value: BigInt.asIntN(64, result), size: cursor - offset }
    }
    shift += 7n
    if (shift >= 70n) throw new Error(`varlong is too big: ${shift}`)
  }
}

function writeVarLong (value, buffer, offset) {
  // FriendlyByteBuf.writeVarLong: `value >>>= 7` is an unsigned shift, so a negative long
  // is its 64-bit pattern and always takes the full ten bytes.
  let x = BigInt(value)
  if (x < -(1n << 63n) || x > ((1n << 63n) - 1n)) throw new RangeError(`varlong out of int64 range: ${x}`)
  x = BigInt.asUintN(64, x)
  let cursor = offset
  while ((x & ~0x7Fn) !== 0n) {
    buffer.writeUInt8(Number(x & 0x7Fn) | 0x80, cursor++)
    x >>= 7n
  }
  buffer.writeUInt8(Number(x), cursor++)
  return cursor
}

function sizeOfVarLong (value) {
  let x = BigInt(value)
  if (x < -(1n << 63n) || x > ((1n << 63n) - 1n)) throw new RangeError(`varlong out of int64 range: ${x}`)
  x = BigInt.asUintN(64, x)
  let size = 1
  while ((x & ~0x7Fn) !== 0n) {
    size++
    x >>= 7n
  }
  return size
}

function readUUID (buffer, offset) {
  if (offset + 16 > buffer.length) { throw new PartialReadError() }
  return {
    value: UUID.stringify(buffer.slice(offset, 16 + offset)),
    size: 16
  }
}

function writeUUID (value, buffer, offset) {
  const buf = value.length === 32 ? Buffer.from(value, 'hex') : UUID.parse(value)
  buf.copy(buffer, offset)
  return offset + 16
}

function sizeOfNbt (value, { tagType } = { tagType: 'nbt' }) {
  return nbt.proto.sizeOf(value, tagType)
}

// Length-prefixed compressed NBT, see differences: http://wiki.vg/index.php?title=Slot_Data&diff=6056&oldid=4753
function readCompressedNbt (buffer, offset) {
  if (offset + 2 > buffer.length) { throw new PartialReadError() }
  const length = buffer.readInt16BE(offset)
  if (length === -1) return { size: 2 }
  if (offset + 2 + length > buffer.length) { throw new PartialReadError() }

  const compressedNbt = buffer.slice(offset + 2, offset + 2 + length)

  let nbtBuffer
  try {
    nbtBuffer = zlib.gunzipSync(compressedNbt) // TODO: async
  } catch (err) {
    throw new PartialReadError('zlib decompress failed: ' + err.message)
  }

  const results = nbt.proto.read(nbtBuffer, 0, 'nbt')
  return {
    size: length + 2,
    value: results.value
  }
}

function writeCompressedNbt (value, buffer, offset) {
  if (value === undefined) {
    buffer.writeInt16BE(-1, offset)
    return offset + 2
  }
  const nbtBuffer = Buffer.alloc(sizeOfNbt(value))
  nbt.proto.write(value, nbtBuffer, 0, 'nbt')

  const compressedNbt = zlib.gzipSync(nbtBuffer) // TODO: async
  compressedNbt.writeUInt8(0, 9) // clear the OS field to match MC

  buffer.writeInt16BE(compressedNbt.length, offset)
  compressedNbt.copy(buffer, offset + 2)
  return offset + 2 + compressedNbt.length
}

function sizeOfCompressedNbt (value) {
  if (value === undefined) { return 2 }

  const nbtBuffer = Buffer.alloc(sizeOfNbt(value, { tagType: 'nbt' }))
  nbt.proto.write(value, nbtBuffer, 0, 'nbt')

  const compressedNbt = zlib.gzipSync(nbtBuffer) // TODO: async

  return 2 + compressedNbt.length
}

function readRestBuffer (buffer, offset) {
  return {
    value: buffer.slice(offset),
    size: buffer.length - offset
  }
}

function writeRestBuffer (value, buffer, offset) {
  value.copy(buffer, offset)
  return offset + value.length
}

function sizeOfRestBuffer (value) {
  return value.length
}

function readEntityMetadata (buffer, offset, { type, endVal }) {
  let cursor = offset
  const metadata = []
  let item
  while (true) {
    if (offset + 1 > buffer.length) { throw new PartialReadError() }
    item = buffer.readUInt8(cursor)
    if (item === endVal) {
      return {
        value: metadata,
        size: cursor + 1 - offset
      }
    }
    const results = this.read(buffer, cursor, type, {})
    metadata.push(results.value)
    cursor += results.size
  }
}

function writeEntityMetadata (value, buffer, offset, { type, endVal }) {
  const self = this
  value.forEach(function (item) {
    offset = self.write(item, buffer, offset, type, {})
  })
  buffer.writeUInt8(endVal, offset)
  return offset + 1
}

function sizeOfEntityMetadata (value, { type }) {
  let size = 1
  for (let i = 0; i < value.length; ++i) {
    size += this.sizeOf(value[i], type, {})
  }
  return size
}

function readTopBitSetTerminatedArray (buffer, offset, { type }) {
  let cursor = offset
  const values = []
  let item
  while (true) {
    if (offset + 1 > buffer.length) { throw new PartialReadError() }
    item = buffer.readUInt8(cursor)
    buffer[cursor] = buffer[cursor] & 127 // removes top bit
    const results = this.read(buffer, cursor, type, {})
    values.push(results.value)
    cursor += results.size
    if ((item & 128) === 0) { // check if top bit is set, if not last value
      return {
        value: values,
        size: cursor - offset
      }
    }
  }
}

function writeTopBitSetTerminatedArray (value, buffer, offset, { type }) {
  const self = this
  let prevOffset = offset
  value.forEach(function (item, i) {
    prevOffset = offset
    offset = self.write(item, buffer, offset, type, {})
    buffer[prevOffset] = i !== value.length - 1 ? (buffer[prevOffset] | 128) : buffer[prevOffset] // set top bit for all values but last
  })
  return offset
}

function sizeOfTopBitSetTerminatedArray (value, { type }) {
  let size = 0
  for (let i = 0; i < value.length; ++i) {
    size += this.sizeOf(value[i], type, {})
  }
  return size
}

// ADDITION OVER UPSTREAM (see header + README.md): upstream
// node-minecraft-protocol does not implement this type. Wire format per
// vanilla 26.3 net/minecraft/network/protocol/game/VecDelta.java and
// ClientboundMoveEntityPacket.java: `varint properties`, stepCount =
// properties >>> 1; if stepCount == 0, three big-endian i16 (xa, ya, za);
// else stepCount steps of `varint ticks` + three big-endian i16.
// Value shape: { properties, linear: { x, y, z } } or
// { properties, steps: [{ ticks, x, y, z }] }.
function readEntityDelta (buffer, offset) {
  const propRes = readVarInt(buffer, offset)
  const properties = propRes.value
  let cursor = offset + propRes.size
  const stepCount = properties >>> 1
  if (stepCount === 0) {
    if (cursor + 6 > buffer.length) { throw new PartialReadError() }
    const value = {
      properties,
      linear: {
        x: buffer.readInt16BE(cursor),
        y: buffer.readInt16BE(cursor + 2),
        z: buffer.readInt16BE(cursor + 4)
      }
    }
    return { value, size: cursor + 6 - offset }
  }
  // Like vanilla's readableBytes/7 guard: refuse step counts that cannot fit.
  const maxSteps = Math.floor((buffer.length - cursor) / 7)
  if (stepCount > maxSteps) { throw new PartialReadError(`VecDelta with size ${stepCount} is bigger than allowed ${maxSteps}`) }
  const steps = []
  for (let i = 0; i < stepCount; i++) {
    const tickRes = readVarInt(buffer, cursor)
    cursor += tickRes.size
    if (cursor + 6 > buffer.length) { throw new PartialReadError() }
    const x = buffer.readInt16BE(cursor)
    const y = buffer.readInt16BE(cursor + 2)
    const z = buffer.readInt16BE(cursor + 4)
    cursor += 6
    steps.push({ ticks: tickRes.value, x, y, z })
  }
  return { value: { properties, steps }, size: cursor - offset }
}

function writeEntityDelta (value, buffer, offset) {
  offset = writeVarInt(value.properties, buffer, offset)
  if (value.steps != null) {
    for (const step of value.steps) {
      offset = writeVarInt(step.ticks, buffer, offset)
      buffer.writeInt16BE(step.x, offset)
      buffer.writeInt16BE(step.y, offset + 2)
      buffer.writeInt16BE(step.z, offset + 4)
      offset += 6
    }
  } else {
    buffer.writeInt16BE(value.linear.x, offset)
    buffer.writeInt16BE(value.linear.y, offset + 2)
    buffer.writeInt16BE(value.linear.z, offset + 4)
    offset += 6
  }
  return offset
}

function sizeOfEntityDelta (value) {
  let size = sizeOfVarInt(value.properties)
  if (value.steps != null) {
    for (const step of value.steps) size += sizeOfVarInt(step.ticks) + 6
  } else {
    size += 6
  }
  return size
}
