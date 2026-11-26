// Runs once in each fresh context, before the script. It takes away what would let a replay take
// another path than the pass it replays (the clock, the time zone, randomness, the weak
// collections), and adds `pipeline` and `parallel`. It gives the engine the function that makes
// a call's failure the `Error` the script catches. Everything it calls once the script has run is
// taken here first, so a script that replaces `Reflect`, `Error`, `Object`, or a prototype's
// method changes nothing it does.
(() => {
  const { apply, construct } = Reflect
  const NativeError = Error
  const fail = sentence => { throw new NativeError(sentence) }
  const refuse = sentence => () => fail(sentence)
  const noClock = refuse(
    'a workflow script has no clock: take the time from `args`, or from a `shell` command')
  const noZone = sentence => refuse(`a workflow script has no time zone: ${sentence}`)

  // `Date()` without `new` gives the current time natively, whatever it is passed, so the
  // constructor itself is replaced, and so is the one the prototype points back to.
  // Every other path that reads the host's time zone throws too: the local getters and setters,
  // the text forms but the ISO, UTC, and JSON ones (string coercion included), a date built from
  // its parts, and every string but the ones `zoned` admits. An object is taken only when it
  // really is a date, as its time value.
  const NativeDate = Date
  const proto = NativeDate.prototype
  const { getTime, valueOf } = proto
  const nativeParse = NativeDate.parse
  const byParts = noZone('build a date from its parts with `new Date(Date.UTC(year, month, ...))`')

  // These read a string by index and compare characters, so no method a script can replace runs.
  const digits = (text, at, count) => {
    for (let index = at; index < at + count; index++) {
      if (!(text[index] >= '0' && text[index] <= '9')) return false
    }
    return true
  }
  const number = (text, at, count) => {
    let value = 0
    for (let index = at; index < at + count; index++) value = value * 10 + (text[index] - 0)
    return value
  }
  const field = (text, at, low, high) =>
    digits(text, at, 2) && number(text, at, 2) >= low && number(text, at, 2) <= high
  const clock = (text, at) =>
    field(text, at, 0, 23) && text[at + 2] === ':' && field(text, at + 3, 0, 59)
  const among = (names, text, at) => {
    for (let index = 0; index < names.length; index += 3) {
      if (names[index] === text[at] && names[index + 1] === text[at + 1] &&
        names[index + 2] === text[at + 2]) return true
    }
    return false
  }
  // An ISO date alone, which the language reads as UTC, or an ISO date and time with `Z` or an
  // offset: `YYYY`, `YYYY-MM`, `YYYY-MM-DD`, then `THH:mm`, `:ss`, `.sss`, and the zone.
  const iso = text => {
    let at = 4
    if (text[0] === '+' || text[0] === '-') {
      if (!digits(text, 1, 6) || (text[0] === '-' && number(text, 1, 6) === 0)) return false
      at = 7
    } else if (!digits(text, 0, 4)) {
      return false
    }
    if (at === text.length) return true
    if (text[at] !== '-' || !field(text, at + 1, 1, 12)) return false
    at += 3
    if (at === text.length) return true
    if (text[at] !== '-' || !field(text, at + 1, 1, 31)) return false
    at += 3
    if (at === text.length) return true
    if (text[at] !== 'T' || !clock(text, at + 1)) return false
    at += 6
    if (text[at] === ':') {
      if (!field(text, at + 1, 0, 59)) return false
      at += 3
      if (text[at] === '.') {
        let end = at + 1
        while (end < at + 4 && digits(text, end, 1)) end++
        if (end === at + 1) return false
        at = end
      }
    }
    if (text[at] === 'Z') return at + 1 === text.length
    return (text[at] === '+' || text[at] === '-') && clock(text, at + 1) && at + 6 === text.length
  }
  // The form `toUTCString` writes: `Thu, 01 Jan 2026 09:00:00 GMT`.
  const utc = text => text.length === 29 && among('SunMonTueWedThuFriSat', text, 0) &&
    text[3] === ',' && text[4] === ' ' && field(text, 5, 1, 31) && text[7] === ' ' &&
    among('JanFebMarAprMayJunJulAugSepOctNovDec', text, 8) && text[11] === ' ' &&
    digits(text, 12, 4) && text[16] === ' ' && clock(text, 17) && text[22] === ':' &&
    field(text, 23, 0, 59) && text[25] === ' ' && text[26] === 'G' && text[27] === 'M' &&
    text[28] === 'T'
  const zoned = text => {
    if (iso(text) || utc(text)) return text
    noZone(`give '${text}' its zone, such as \`Z\` or \`+02:00\``)()
  }
  function ScriptDate(...values) {
    if (new.target === undefined || values.length === 0) noClock()
    if (values.length > 1) byParts()
    let value = values[0]
    if (typeof value === 'string') {
      zoned(value)
    } else if (typeof value === 'function' || (typeof value === 'object' && value !== null)) {
      try {
        value = apply(getTime, value, [])
      } catch {
        fail('a date is built from milliseconds, a string with its zone, or another date')
      }
    }
    return construct(NativeDate, [value], new.target)
  }
  ScriptDate.prototype = proto
  ScriptDate.parse = text => {
    if (typeof text !== 'string') fail('a date is parsed from a string with its zone')
    return apply(nativeParse, NativeDate, [zoned(text)])
  }
  ScriptDate.UTC = NativeDate.UTC
  ScriptDate.now = noClock
  const hidden = value => ({ value, writable: true, configurable: true })
  Object.defineProperty(proto, 'constructor', hidden(ScriptDate))
  Object.defineProperty(globalThis, 'Date', hidden(ScriptDate))
  for (const part of ['Date', 'Day', 'FullYear', 'Hours', 'Milliseconds', 'Minutes', 'Month',
    'Seconds']) {
    for (const verb of part === 'Day' ? ['get'] : ['get', 'set']) {
      const name = verb + part
      Object.defineProperty(proto, name,
        hidden(noZone(`use \`${verb}UTC${part}\` in place of \`${name}\``)))
    }
  }
  for (const [name, utc] of [['getYear', 'getUTCFullYear'], ['setYear', 'setUTCFullYear']]) {
    if (name in proto) {
      Object.defineProperty(proto, name, hidden(noZone(`use \`${utc}\` in place of \`${name}\``)))
    }
  }
  Object.defineProperty(proto, 'getTimezoneOffset',
    hidden(noZone('every date is read in UTC, which has no offset')))
  const asText = noZone('write a date as text with `toISOString` or `toUTCString`')
  for (const name of ['toString', 'toDateString', 'toTimeString', 'toLocaleString',
    'toLocaleDateString', 'toLocaleTimeString']) {
    Object.defineProperty(proto, name, hidden(asText))
  }
  Object.defineProperty(proto, Symbol.toPrimitive, {
    value(hint) {
      if (hint !== 'number') asText()
      return apply(valueOf, this, [])
    },
    configurable: true,
  })
  Math.random = refuse('a workflow script has no randomness: take a seed from `args`')

  // QuickJS-NG 0.16.2 lacks the WeakMap cleanup use-after-free fix of 0.18.0.
  delete globalThis.WeakMap
  delete globalThis.WeakSet

  const settle = async task => {
    try {
      return await task()
    } catch {
      return null
    }
  }
  globalThis.pipeline = (items, ...stages) => Promise.all(Array.from(items, (item, index) =>
    settle(async () => {
      let value = item
      for (const stage of stages) value = await stage(value, item, index)
      return value
    })))
  globalThis.parallel = tasks => Promise.all(Array.from(tasks, settle))

  const { assign } = Object
  return fields => assign(new NativeError(fields.reason), fields)
})()
