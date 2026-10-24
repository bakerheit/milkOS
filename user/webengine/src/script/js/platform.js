// ---- crypto.subtle (digests and HMAC), scheduler, constructable style sheets, navigator.locks ---------------
// ---- and credentials, small Document / Element members, a few more events -------------------------------------

// ---- SHA-1, SHA-2 -------------------------------------------------------------------------------------------

const K256 = new Uint32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);
// The low halves of the 64-bit constants (the high halves are K256's, and 16 more).
const K512L = new Uint32Array([
  0xd728ae22, 0x23ef65cd, 0xec4d3b2f, 0x8189dbbc, 0xf348b538, 0xb605d019, 0xaf194f9b, 0xda6d8118, 0xa3030242, 0x45706fbe, 0x4ee4b28c, 0xd5ffb4e2, 0xf27b896f, 0x3b1696b1, 0x25c71235, 0xcf692694,
  0x9ef14ad2, 0x384f25e3, 0x8b8cd5b5, 0x77ac9c65, 0x592b0275, 0x6ea6e483, 0xbd41fbd4, 0x831153b5, 0xee66dfab, 0x2db43210, 0x98fb213f, 0xbeef0ee4, 0x3da88fc2, 0x930aa725, 0xe003826f, 0x0a0e6e70,
  0x46d22ffc, 0x5c26c926, 0x5ac42aed, 0x9d95b3df, 0x8baf63de, 0x3c77b2a8, 0x47edaee6, 0x1482353b, 0x4cf10364, 0xbc423001, 0xd0f89791, 0x0654be30, 0xd6ef5218, 0x5565a910, 0x5771202a, 0x32bbd1b8,
  0xb8d2d0c8, 0x5141ab53, 0xdf8eeb99, 0xe19b48a8, 0xc5c95a63, 0xe3418acb, 0x7763e373, 0xd6b2b8a3, 0x5defb2fc, 0x43172f60, 0xa1f0ab72, 0x1a6439ec, 0x23631e28, 0xde82bde9, 0xb2c67915, 0xe372532b,
  0xea26619c, 0x21c0c207, 0xcde0eb1e, 0xee6ed178, 0x72176fba, 0xa2c898a6, 0xbef90dae, 0x131c471b, 0x23047d84, 0x40c72493, 0x15c9bebc, 0x9c100d4c, 0xcb3e42b6, 0xfc657e2a, 0x3ad6faec, 0x4a475817,
]);
const K512H = new Uint32Array([
  ...K256,
  0xca273ece, 0xd186b8c7, 0xeada7dd6, 0xf57d4f7f, 0x06f067aa, 0x0a637dc5, 0x113f9804, 0x1b710b35, 0x28db77f5, 0x32caab7b, 0x3c9ebe0a, 0x431d67c4, 0x4cc5d4be, 0x597f299c, 0x5fcb6fab, 0x6c44198c,
]);

function shaPad(msg, block, lenBytes) {
  const len = msg.length;
  const total = Math.ceil((len + 1 + lenBytes) / block) * block;
  const buf = new Uint8Array(total);
  buf.set(msg);
  buf[len] = 0x80;
  const dv = new DataView(buf.buffer);
  dv.setUint32(total - 8, Math.floor(len / 0x20000000) >>> 0);
  dv.setUint32(total - 4, (len * 8) % 4294967296);
  return dv;
}
const rotl = (x, n) => (x << n) | (x >>> (32 - n));
const rotr = (x, n) => (x >>> n) | (x << (32 - n));

function sha1(msg) {
  const dv = shaPad(msg, 64, 8);
  let h0 = 0x67452301, h1 = 0xefcdab89, h2 = 0x98badcfe, h3 = 0x10325476, h4 = 0xc3d2e1f0;
  const w = new Uint32Array(80);
  for (let off = 0; off < dv.byteLength; off += 64) {
    for (let t = 0; t < 16; t++) w[t] = dv.getUint32(off + 4 * t);
    for (let t = 16; t < 80; t++) w[t] = rotl(w[t - 3] ^ w[t - 8] ^ w[t - 14] ^ w[t - 16], 1);
    let a = h0, b = h1, c = h2, d = h3, e = h4;
    for (let t = 0; t < 80; t++) {
      let f, k;
      if (t < 20) { f = (b & c) | (~b & d); k = 0x5a827999; }
      else if (t < 40) { f = b ^ c ^ d; k = 0x6ed9eba1; }
      else if (t < 60) { f = (b & c) | (b & d) | (c & d); k = 0x8f1bbcdc; }
      else { f = b ^ c ^ d; k = 0xca62c1d6; }
      const tmp = (rotl(a, 5) + f + e + k + w[t]) >>> 0;
      e = d; d = c; c = rotl(b, 30); b = a; a = tmp;
    }
    h0 = (h0 + a) >>> 0; h1 = (h1 + b) >>> 0; h2 = (h2 + c) >>> 0; h3 = (h3 + d) >>> 0; h4 = (h4 + e) >>> 0;
  }
  const out = new Uint8Array(20);
  const o = new DataView(out.buffer);
  [h0, h1, h2, h3, h4].forEach((h, i) => o.setUint32(4 * i, h));
  return out;
}

function sha256(msg) {
  const dv = shaPad(msg, 64, 8);
  const H = new Uint32Array([0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19]);
  const w = new Uint32Array(64);
  for (let off = 0; off < dv.byteLength; off += 64) {
    for (let t = 0; t < 16; t++) w[t] = dv.getUint32(off + 4 * t);
    for (let t = 16; t < 64; t++) {
      const s0 = rotr(w[t - 15], 7) ^ rotr(w[t - 15], 18) ^ (w[t - 15] >>> 3);
      const s1 = rotr(w[t - 2], 17) ^ rotr(w[t - 2], 19) ^ (w[t - 2] >>> 10);
      w[t] = (w[t - 16] + s0 + w[t - 7] + s1) >>> 0;
    }
    let a = H[0], b = H[1], c = H[2], d = H[3], e = H[4], f = H[5], g = H[6], h = H[7];
    for (let t = 0; t < 64; t++) {
      const S1 = rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25);
      const ch = (e & f) ^ (~e & g);
      const t1 = (h + S1 + ch + K256[t] + w[t]) >>> 0;
      const S0 = rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22);
      const maj = (a & b) ^ (a & c) ^ (b & c);
      const t2 = (S0 + maj) >>> 0;
      h = g; g = f; f = e; e = (d + t1) >>> 0; d = c; c = b; b = a; a = (t1 + t2) >>> 0;
    }
    H[0] += a; H[1] += b; H[2] += c; H[3] += d; H[4] += e; H[5] += f; H[6] += g; H[7] += h;
  }
  const out = new Uint8Array(32);
  const o = new DataView(out.buffer);
  for (let i = 0; i < 8; i++) o.setUint32(4 * i, H[i]);
  return out;
}

// 64-bit words as (hi, lo) pairs.
const SHA512_INIT = [
  [0x6a09e667, 0xf3bcc908], [0xbb67ae85, 0x84caa73b], [0x3c6ef372, 0xfe94f82b], [0xa54ff53a, 0x5f1d36f1],
  [0x510e527f, 0xade682d1], [0x9b05688c, 0x2b3e6c1f], [0x1f83d9ab, 0xfb41bd6b], [0x5be0cd19, 0x137e2179],
];
const SHA384_INIT = [
  [0xcbbb9d5d, 0xc1059ed8], [0x629a292a, 0x367cd507], [0x9159015a, 0x3070dd17], [0x152fecd8, 0xf70e5939],
  [0x67332667, 0xffc00b31], [0x8eb44a87, 0x68581511], [0xdb0c2e0d, 0x64f98fa7], [0x47b5481d, 0xbefa4fa4],
];
function sha512(msg, init, outLen) {
  const dv = shaPad(msg, 128, 16);
  const Hh = new Uint32Array(8);
  const Hl = new Uint32Array(8);
  for (let i = 0; i < 8; i++) { Hh[i] = init[i][0]; Hl[i] = init[i][1]; }
  const Wh = new Uint32Array(80);
  const Wl = new Uint32Array(80);
  // Sums of up to five 64-bit values: the low halves as doubles, the carry into the high halves.
  for (let off = 0; off < dv.byteLength; off += 128) {
    for (let t = 0; t < 16; t++) { Wh[t] = dv.getUint32(off + 8 * t); Wl[t] = dv.getUint32(off + 8 * t + 4); }
    for (let t = 16; t < 80; t++) {
      const h15 = Wh[t - 15], l15 = Wl[t - 15], h2 = Wh[t - 2], l2 = Wl[t - 2];
      // s0 = rotr1 ^ rotr8 ^ shr7
      const s0h = ((h15 >>> 1) | (l15 << 31)) ^ ((h15 >>> 8) | (l15 << 24)) ^ (h15 >>> 7);
      const s0l = ((l15 >>> 1) | (h15 << 31)) ^ ((l15 >>> 8) | (h15 << 24)) ^ ((l15 >>> 7) | (h15 << 25));
      // s1 = rotr19 ^ rotr61 ^ shr6
      const s1h = ((h2 >>> 19) | (l2 << 13)) ^ ((l2 >>> 29) | (h2 << 3)) ^ (h2 >>> 6);
      const s1l = ((l2 >>> 19) | (h2 << 13)) ^ ((h2 >>> 29) | (l2 << 3)) ^ ((l2 >>> 6) | (h2 << 26));
      const lo = (s0l >>> 0) + (s1l >>> 0) + Wl[t - 16] + Wl[t - 7];
      Wl[t] = lo >>> 0;
      Wh[t] = ((s0h >>> 0) + (s1h >>> 0) + Wh[t - 16] + Wh[t - 7] + Math.floor(lo / 4294967296)) >>> 0;
    }
    let ah = Hh[0], al = Hl[0], bh = Hh[1], bl = Hl[1], ch = Hh[2], cl = Hl[2], dh = Hh[3], dl = Hl[3];
    let eh = Hh[4], el = Hl[4], fh = Hh[5], fl = Hl[5], gh = Hh[6], gl = Hl[6], hh = Hh[7], hl = Hl[7];
    for (let t = 0; t < 80; t++) {
      // S1 = rotr14 ^ rotr18 ^ rotr41
      const S1h = ((eh >>> 14) | (el << 18)) ^ ((eh >>> 18) | (el << 14)) ^ ((el >>> 9) | (eh << 23));
      const S1l = ((el >>> 14) | (eh << 18)) ^ ((el >>> 18) | (eh << 14)) ^ ((eh >>> 9) | (el << 23));
      const chh = (eh & fh) ^ (~eh & gh);
      const chl = (el & fl) ^ (~el & gl);
      let lo = (hl >>> 0) + (S1l >>> 0) + (chl >>> 0) + K512L[t] + Wl[t];
      const t1l = lo >>> 0;
      const t1h = ((hh >>> 0) + (S1h >>> 0) + (chh >>> 0) + K512H[t] + Wh[t] + Math.floor(lo / 4294967296)) >>> 0;
      // S0 = rotr28 ^ rotr34 ^ rotr39
      const S0h = ((ah >>> 28) | (al << 4)) ^ ((al >>> 2) | (ah << 30)) ^ ((al >>> 7) | (ah << 25));
      const S0l = ((al >>> 28) | (ah << 4)) ^ ((ah >>> 2) | (al << 30)) ^ ((ah >>> 7) | (al << 25));
      const mjh = (ah & bh) ^ (ah & ch) ^ (bh & ch);
      const mjl = (al & bl) ^ (al & cl) ^ (bl & cl);
      lo = (S0l >>> 0) + (mjl >>> 0);
      const t2l = lo >>> 0;
      const t2h = ((S0h >>> 0) + (mjh >>> 0) + Math.floor(lo / 4294967296)) >>> 0;
      hh = gh; hl = gl; gh = fh; gl = fl; fh = eh; fl = el;
      lo = dl + t1l;
      el = lo >>> 0;
      eh = (dh + t1h + Math.floor(lo / 4294967296)) >>> 0;
      dh = ch; dl = cl; ch = bh; cl = bl; bh = ah; bl = al;
      lo = t1l + t2l;
      al = lo >>> 0;
      ah = (t1h + t2h + Math.floor(lo / 4294967296)) >>> 0;
    }
    const add = (i, vh, vl) => {
      const lo = Hl[i] + (vl >>> 0);
      Hl[i] = lo >>> 0;
      Hh[i] = (Hh[i] + (vh >>> 0) + Math.floor(lo / 4294967296)) >>> 0;
    };
    add(0, ah, al); add(1, bh, bl); add(2, ch, cl); add(3, dh, dl); add(4, eh, el); add(5, fh, fl); add(6, gh, gl); add(7, hh, hl);
  }
  const out = new Uint8Array(64);
  const o = new DataView(out.buffer);
  for (let i = 0; i < 8; i++) { o.setUint32(8 * i, Hh[i]); o.setUint32(8 * i + 4, Hl[i]); }
  return out.subarray(0, outLen);
}

const HASHES = {
  'SHA-1': { f: sha1, block: 64 },
  'SHA-256': { f: sha256, block: 64 },
  'SHA-384': { f: (m) => sha512(m, SHA384_INIT, 48), block: 128 },
  'SHA-512': { f: (m) => sha512(m, SHA512_INIT, 64), block: 128 },
};
function hmac(hash, key, msg) {
  const h = HASHES[hash];
  let k = key.length > h.block ? h.f(key) : key;
  const pad = new Uint8Array(h.block);
  pad.set(k);
  const inner = new Uint8Array(h.block + msg.length);
  const outer = new Uint8Array(h.block + (hash === 'SHA-1' ? 20 : hash === 'SHA-256' ? 32 : hash === 'SHA-384' ? 48 : 64));
  for (let i = 0; i < h.block; i++) {
    inner[i] = pad[i] ^ 0x36;
    outer[i] = pad[i] ^ 0x5c;
  }
  inner.set(msg, h.block);
  outer.set(h.f(inner), h.block);
  return h.f(outer);
}

const CRYPTO_MAX = 8 << 20;
function cryptoBytes(data, what) {
  let u;
  if (data instanceof ArrayBuffer) u = new Uint8Array(data);
  else if (ArrayBuffer.isView(data)) u = new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
  else throw new TypeError("Failed to execute '" + what + "' on 'SubtleCrypto': parameter 2 is not of type '(ArrayBuffer or ArrayBufferView)'.");
  if (u.length > CRYPTO_MAX) throw domError('OperationError', 'The data is too big for this browser to process (' + CRYPTO_MAX + ' bytes at most).');
  return u;
}
function algName(a) {
  const n = isObj(a) ? a.name : a;
  if (n === undefined) throw new TypeError('Algorithm: Not an object or string');
  return String(n).toUpperCase();
}
function hashName(h) {
  const n = algName(h);
  if (!HASHES[n]) throw domError('NotSupportedError', 'Unrecognized name.');
  return n;
}
const rejectWith = (e) => Promise.reject(e);

class CryptoKey {
  constructor() {
    throw new TypeError('Illegal constructor');
  }
  get type() { return slots(this, 'CryptoKey').type; }
  get extractable() { return slots(this, 'CryptoKey').extractable; }
  get algorithm() { return slots(this, 'CryptoKey').algorithm; }
  get usages() { return slots(this, 'CryptoKey').usages.slice(); }
}
tag(CryptoKey);
enumerable(CryptoKey, ['type', 'extractable', 'algorithm', 'usages']);
function makeKey(bytes, hash, extractable, usages) {
  const k = Object.create(CryptoKey.prototype);
  setSlots(k, { brand: 'CryptoKey', type: 'secret', extractable: !!extractable, algorithm: { name: 'HMAC', hash: { name: hash }, length: bytes.length * 8 }, usages: Array.from(usages || []).map(String), bytes: bytes.slice() });
  return k;
}
const ab = (u8) => u8.buffer.slice(u8.byteOffset, u8.byteOffset + u8.byteLength);

class SubtleCrypto {
  constructor() {
    throw new TypeError('Illegal constructor');
  }
  digest(algorithm, data) {
    try {
      need(arguments, 2, 'digest', 'SubtleCrypto');
      const n = hashName(algorithm);
      const bytes = cryptoBytes(data, 'digest');
      return Promise.resolve(ab(HASHES[n].f(bytes)));
    } catch (e) {
      return rejectWith(e);
    }
  }
  importKey(format, keyData, algorithm, extractable, usages) {
    try {
      need(arguments, 5, 'importKey', 'SubtleCrypto');
      if (algName(algorithm) !== 'HMAC') throw domError('NotSupportedError', 'Unrecognized name.');
      if (String(format) !== 'raw') throw domError('NotSupportedError', 'Only the "raw" format is supported.');
      const hash = hashName(algorithm.hash);
      return Promise.resolve(makeKey(cryptoBytes(keyData, 'importKey'), hash, extractable, usages));
    } catch (e) {
      return rejectWith(e);
    }
  }
  exportKey(format, key) {
    try {
      const s = slots(key, 'CryptoKey');
      if (!s.extractable) throw domError('InvalidAccessError', 'key is not extractable');
      if (String(format) !== 'raw') throw domError('NotSupportedError', 'Only the "raw" format is supported.');
      return Promise.resolve(ab(s.bytes));
    } catch (e) {
      return rejectWith(e);
    }
  }
  generateKey(algorithm, extractable, usages) {
    try {
      if (algName(algorithm) !== 'HMAC') throw domError('NotSupportedError', 'Unrecognized name.');
      const hash = hashName(algorithm.hash);
      const len = Math.ceil((algorithm.length || HASHES[hash].block * 8) / 8);
      const bytes = new Uint8Array(len);
      randomFill(bytes);
      return Promise.resolve(makeKey(bytes, hash, extractable, usages));
    } catch (e) {
      return rejectWith(e);
    }
  }
  sign(algorithm, key, data) {
    try {
      need(arguments, 3, 'sign', 'SubtleCrypto');
      const s = slots(key, 'CryptoKey');
      if (algName(algorithm) !== 'HMAC' || s.algorithm.name !== 'HMAC') throw domError('InvalidAccessError', 'The requested operation is not valid for the provided key.');
      if (s.usages.indexOf('sign') < 0) throw domError('InvalidAccessError', "key.usages does not permit this operation");
      return Promise.resolve(ab(hmac(s.algorithm.hash.name, s.bytes, cryptoBytes(data, 'sign'))));
    } catch (e) {
      return rejectWith(e);
    }
  }
  verify(algorithm, key, signature, data) {
    try {
      need(arguments, 4, 'verify', 'SubtleCrypto');
      const s = slots(key, 'CryptoKey');
      if (algName(algorithm) !== 'HMAC' || s.algorithm.name !== 'HMAC') throw domError('InvalidAccessError', 'The requested operation is not valid for the provided key.');
      if (s.usages.indexOf('verify') < 0) throw domError('InvalidAccessError', "key.usages does not permit this operation");
      const want = hmac(s.algorithm.hash.name, s.bytes, cryptoBytes(data, 'verify'));
      const got = cryptoBytes(signature, 'verify');
      let diff = want.length ^ got.length;
      for (let i = 0; i < want.length; i++) diff |= want[i] ^ (got[i] || 0);
      return Promise.resolve(diff === 0);
    } catch (e) {
      return rejectWith(e);
    }
  }
}
for (const m of ['encrypt', 'decrypt', 'deriveBits', 'deriveKey', 'wrapKey', 'unwrapKey']) {
  ObjDefine(SubtleCrypto.prototype, m, {
    value: function () {
      return rejectWith(domError('NotSupportedError', 'Algorithm: Unrecognized name'));
    },
    writable: true,
    configurable: true,
    enumerable: true,
  });
}
tag(SubtleCrypto);
enumerable(SubtleCrypto, ['digest', 'importKey', 'exportKey', 'generateKey', 'sign', 'verify']);
const subtleObject = Object.create(SubtleCrypto.prototype);
{
  // (Only a secure context has `subtle`.)
  const d = Object.getOwnPropertyDescriptor(Crypto.prototype, 'subtle');
  ObjDefine(Crypto.prototype, 'subtle', { get() { return G.isSecureContext ? subtleObject : undefined; }, enumerable: true, configurable: true });
}

// ---- scheduler -----------------------------------------------------------------------------------------------

class Scheduler {
  constructor() {
    throw new TypeError('Illegal constructor');
  }
  postTask(callback, options) {
    if (typeof callback !== 'function') return rejectWith(new TypeError("Failed to execute 'postTask' on 'Scheduler': The callback provided as parameter 1 is not a function."));
    const o = isObj(options) ? options : {};
    const signal = o.signal;
    if (signal && signal.aborted) return rejectWith(signal.reason !== undefined ? signal.reason : domError('AbortError', 'The task was aborted.'));
    const delay = Number(o.delay) > 0 ? Number(o.delay) : 0;
    return new Promise((resolve, reject) => {
      let id;
      const onAbort = () => {
        native.clearTimer(id);
        reject(signal.reason !== undefined ? signal.reason : domError('AbortError', 'The task was aborted.'));
      };
      if (signal) signal.addEventListener('abort', onAbort, { once: true });
      id = native.setTimer(() => {
        if (signal) signal.removeEventListener('abort', onAbort);
        try {
          resolve(callback());
        } catch (e) {
          reject(e);
        }
      }, delay);
    });
  }
  yield() {
    return new Promise((resolve) => native.setTimer(resolve, 0));
  }
}
tag(Scheduler);
enumerable(Scheduler, ['postTask', 'yield']);
const schedulerObject = Object.create(Scheduler.prototype);

// ---- constructable style sheets ------------------------------------------------------------------------------
// The rules are kept as text and can be read back; they are not applied to the page.

class CSSStyleSheet {
  constructor(options) {
    setSlots(this, { brand: 'CSSStyleSheet', rules: [], disabled: false, media: isObj(options) && options.media !== undefined ? String(options.media) : '' });
  }
  get cssRules() { return slots(this, 'CSSStyleSheet').rules.map((t) => ({ cssText: t, type: 1 })); }
  get rules() { return this.cssRules; }
  get disabled() { return slots(this, 'CSSStyleSheet').disabled; }
  set disabled(v) { slots(this, 'CSSStyleSheet').disabled = !!v; }
  get ownerNode() { return null; }
  get parentStyleSheet() { return null; }
  get href() { return null; }
  get type() { return 'text/css'; }
  get title() { return null; }
  insertRule(rule, index) {
    need(arguments, 1, 'insertRule', 'CSSStyleSheet');
    const s = slots(this, 'CSSStyleSheet');
    const i = index === undefined ? 0 : index >>> 0;
    if (i > s.rules.length) throw domError('IndexSizeError', "Failed to execute 'insertRule' on 'CSSStyleSheet': The index provided (" + i + ") is larger than the maximum index (" + s.rules.length + ').');
    if (s.rules.length >= 100000) throw domError('QuotaExceededError', 'Too many rules.');
    s.rules.splice(i, 0, String(rule));
    return i;
  }
  deleteRule(index) {
    const s = slots(this, 'CSSStyleSheet');
    const i = index >>> 0;
    if (i >= s.rules.length) throw domError('IndexSizeError', "Failed to execute 'deleteRule' on 'CSSStyleSheet': The index provided (" + i + ') is larger than the maximum index (' + (s.rules.length - 1) + ').');
    s.rules.splice(i, 1);
  }
  replaceSync(text) {
    const s = slots(this, 'CSSStyleSheet');
    s.rules = String(text).split(/}\s*/).map((t) => t.trim()).filter(Boolean).map((t) => t + ' }');
  }
  replace(text) {
    try {
      this.replaceSync(text);
      return Promise.resolve(this);
    } catch (e) {
      return rejectWith(e);
    }
  }
}
tag(CSSStyleSheet);
const sheetList = [];
ObjDefine(sheetList, TAG, { value: 'StyleSheetList', configurable: true });
ObjDefine(sheetList, 'item', { value: (i) => sheetList[i] || null, configurable: true });

// ---- navigator.locks, credentials, userActivation ---------------------------------------------------------------

class LockManager {
  constructor() {
    throw new TypeError('Illegal constructor');
  }
  request(name, options, callback) {
    if (callback === undefined) {
      callback = options;
      options = {};
    }
    if (typeof callback !== 'function') return rejectWith(new TypeError("Failed to execute 'request' on 'LockManager': parameter 3 is not of type 'Function'."));
    name = String(name);
    const o = isObj(options) ? options : {};
    const mode = o.mode === 'shared' ? 'shared' : 'exclusive';
    const held = lockQueue.get(name) || { tail: Promise.resolve(), holders: 0 };
    if (o.ifAvailable && held.holders > 0) return Promise.resolve().then(() => callback(null));
    lockQueue.set(name, held);
    held.holders++;
    const run = held.tail.then(() => callback({ name, mode }));
    held.tail = run.then(() => {}, () => {});
    held.tail.then(() => {
      if (--held.holders === 0) lockQueue.delete(name);
    });
    return run;
  }
  query() {
    return Promise.resolve({ held: [], pending: [] });
  }
}
tag(LockManager);
enumerable(LockManager, ['request', 'query']);
const lockQueue = new Map();
const lockManager = Object.create(LockManager.prototype);

class CredentialsContainer {
  constructor() {
    throw new TypeError('Illegal constructor');
  }
  get() { return Promise.resolve(null); }
  store(c) { return Promise.resolve(c); }
  create() { return rejectWith(domError('NotSupportedError', 'Credential creation is not supported.')); }
  preventSilentAccess() { return Promise.resolve(); }
}
tag(CredentialsContainer);
enumerable(CredentialsContainer, ['get', 'store', 'create', 'preventSilentAccess']);
const credentialsContainer = Object.create(CredentialsContainer.prototype);

class UserActivation {
  constructor() {
    throw new TypeError('Illegal constructor');
  }
  get isActive() { return native.userActivation()[0]; }
  get hasBeenActive() { return native.userActivation()[1]; }
}
tag(UserActivation);
enumerable(UserActivation, ['isActive', 'hasBeenActive']);
const userActivation = Object.create(UserActivation.prototype);

// ---- Document and Element members ---------------------------------------------------------------------------------

function elementsFromPoint(x, y) {
  const out = [];
  let el = doc().elementFromPoint(x, y);
  while (el) {
    out.push(el);
    el = el.parentElement;
  }
  return out;
}
function lastModified() {
  const d = new Date(native.time());
  const p = (n) => (n < 10 ? '0' : '') + n;
  return p(d.getMonth() + 1) + '/' + p(d.getDate()) + '/' + d.getFullYear() + ' ' + p(d.getHours()) + ':' + p(d.getMinutes()) + ':' + p(d.getSeconds());
}
function noFullscreen() {
  return rejectWith(new TypeError('Fullscreen is not supported.'));
}
const isTag = (el, names) => el && el.nodeType === 1 && names.indexOf(el.localName) >= 0;
function mediaOnly(el, what) {
  if (!isTag(el, ['audio', 'video'])) throw new TypeError("Failed to execute '" + what + "' on 'HTMLMediaElement': Illegal invocation");
}
function mediaPlay() {
  mediaOnly(this, 'play');
  return rejectWith(domError('NotSupportedError', 'The media could not be played: there is no audio or video output.'));
}
function mediaPause() { mediaOnly(this, 'pause'); }
function mediaLoad() { mediaOnly(this, 'load'); }
function mediaCanPlayType() { mediaOnly(this, 'canPlayType'); return ''; }
function dialogOnly(el, what) {
  if (!isTag(el, ['dialog'])) throw new TypeError("Failed to execute '" + what + "' on 'HTMLDialogElement': Illegal invocation");
}
function dialogShow() { dialogOnly(this, 'show'); this.setAttribute('open', ''); }
function dialogShowModal() {
  dialogOnly(this, 'showModal');
  if (this.hasAttribute('open')) throw domError('InvalidStateError', "Failed to execute 'showModal' on 'HTMLDialogElement': The element already has an 'open' attribute, and therefore cannot be opened modally.");
  this.setAttribute('open', '');
}
function dialogClose(value) {
  dialogOnly(this, 'close');
  if (!this.hasAttribute('open')) return;
  this.removeAttribute('open');
  if (value !== undefined) this.returnValue = String(value);
  nextTick(() => this.dispatchEvent(new G.Event('close')));
}
function imageDecode() {
  if (!isTag(this, ['img'])) return rejectWith(new TypeError("Failed to execute 'decode' on 'HTMLImageElement': Illegal invocation"));
  if (this.complete && this.naturalWidth > 0) return Promise.resolve();
  return new Promise((resolve, reject) => {
    this.addEventListener('load', () => resolve(), { once: true });
    this.addEventListener('error', () => reject(domError('EncodingError', 'The source image cannot be decoded.')), { once: true });
  });
}
function checkVisibility(options) {
  if (!this.isConnected || this.getClientRects().length === 0) return false;
  const cs = G.getComputedStyle(this);
  if (isObj(options) && (options.checkVisibilityCSS || options.visibilityProperty) && cs.visibility !== 'visible') return false;
  if (isObj(options) && (options.checkOpacity || options.opacityProperty) && cs.opacity === '0') return false;
  return true;
}
const noop = function () {};
const designMode = 'off';
const docDir = '';

// ---- more events ----------------------------------------------------------------------------------------------

class FormDataEvent extends G.Event {
  constructor(type, init) {
    eventNeeds(arguments, 'FormDataEvent');
    super(type, init);
    if (!isObj(init) || !(init.formData instanceof G.FormData)) throw new TypeError("Failed to construct 'FormDataEvent': Failed to read the 'formData' property from 'FormDataEventInit': Required member is undefined.");
    eventMembers(this, init, { formData: null });
  }
}
tag(FormDataEvent);
class ToggleEvent extends G.Event {
  constructor(type, init) {
    eventNeeds(arguments, 'ToggleEvent');
    super(type, init);
    eventMembers(this, init, { oldState: '', newState: '' });
  }
}
tag(ToggleEvent);
class SecurityPolicyViolationEvent extends G.Event {
  constructor(type, init) {
    eventNeeds(arguments, 'SecurityPolicyViolationEvent');
    super(type, init);
    eventMembers(this, init, { documentURI: '', referrer: '', blockedURI: '', violatedDirective: '', effectiveDirective: '', originalPolicy: '', disposition: 'enforce', sourceFile: '', sample: '', statusCode: 0, lineNumber: 0, columnNumber: 0 });
  }
}
tag(SecurityPolicyViolationEvent);
class DeviceOrientationEvent extends G.Event {
  constructor(type, init) {
    eventNeeds(arguments, 'DeviceOrientationEvent');
    super(type, init);
    eventMembers(this, init, { alpha: null, beta: null, gamma: null, absolute: false });
  }
}
tag(DeviceOrientationEvent);
class DeviceMotionEvent extends G.Event {
  constructor(type, init) {
    eventNeeds(arguments, 'DeviceMotionEvent');
    super(type, init);
    eventMembers(this, init, { acceleration: null, accelerationIncludingGravity: null, rotationRate: null, interval: 0 });
  }
}
tag(DeviceMotionEvent);
class GamepadEvent extends G.Event {
  constructor(type, init) {
    eventNeeds(arguments, 'GamepadEvent');
    super(type, init);
    eventMembers(this, init, { gamepad: null });
  }
}
tag(GamepadEvent);
