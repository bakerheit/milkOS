
// ---- Intl --------------------------------------------------------------------------------------------------
// The parts of ECMA-402 that pages use, for English: number, date and time, plural, list and
// relative-time formatting, collation, display names and segmentation. Any locale is accepted and
// formatted as en-US (resolvedOptions().locale says "en-US"); there are no other languages' data.

const INTL_LOCALE = 'en-US';

function canonLocale(tag) {
  if (typeof tag !== 'string' && !(isObj(tag) && typeof tag.toString === 'function')) throw new TypeError('Incorrect locale information provided');
  tag = String(tag);
  if (!/^[A-Za-z]{2,8}(-[A-Za-z0-9]{1,8})*$/.test(tag)) throw new RangeError('Incorrect locale information provided');
  const p = tag.split('-');
  const out = [p[0].toLowerCase()];
  for (let i = 1; i < p.length; i++) {
    const s = p[i];
    if (s.length === 4 && /^[A-Za-z]+$/.test(s) && i === 1) out.push(s[0].toUpperCase() + s.slice(1).toLowerCase());
    else if ((s.length === 2 && /^[A-Za-z]+$/.test(s)) || (s.length === 3 && /^[0-9]+$/.test(s))) out.push(s.toUpperCase());
    else out.push(s.toLowerCase());
  }
  return out.join('-');
}
function canonLocales(l) {
  if (l === undefined) return [];
  if (typeof l === 'string' || hasBrand(l, 'Locale')) return [canonLocale(l)];
  const out = [];
  if (!isObj(l)) throw new TypeError('Incorrect locale information provided');
  const n = l.length >>> 0;
  for (let i = 0; i < n; i++) {
    if (i in l) {
      const c = canonLocale(l[i]);
      if (out.indexOf(c) < 0) out.push(c);
    }
  }
  return out;
}
function resolveLocale(l) {
  // en-anything formats as en-US; other languages are not available, so they fall back to it.
  const list = canonLocales(l);
  for (const c of list) if (/^en(-|$)/.test(c)) return c.indexOf('-') < 0 ? 'en' : c.split('-u-')[0];
  return INTL_LOCALE;
}
function getOpts(o) {
  if (o === undefined) return Object.create(null);
  if (o === null) throw new TypeError('Cannot convert undefined or null to object');
  return Object(o);
}
function optStr(o, name, allowed, dflt) {
  let v = o[name];
  if (v === undefined) return dflt;
  v = String(v);
  if (allowed && allowed.indexOf(v) < 0) throw new RangeError('Value ' + v + ' out of range for Intl options property ' + name);
  return v;
}
function optBool(o, name, dflt) {
  const v = o[name];
  return v === undefined ? dflt : Boolean(v);
}
function optNum(o, name, min, max, dflt) {
  let v = o[name];
  if (v === undefined) return dflt;
  v = Number(v);
  if (v !== v || v < min || v > max) throw new RangeError(name + ' value is out of range.');
  return Math.floor(v);
}

// ---- decimal digits ----

// The shortest decimal digits of a finite x >= 0: value = 0.DIGITS * 10^point.
function decimalOf(x) {
  const m = /^(\d+)(?:\.(\d+))?(?:e([+-]\d+))?$/.exec(String(x));
  const int = m[1], frac = m[2] || '', e = m[3] ? parseInt(m[3], 10) : 0;
  let digits = int + frac;
  let point = int.length + e;
  let lead = 0;
  while (lead < digits.length - 1 && digits.charCodeAt(lead) === 48) lead++;
  digits = digits.slice(lead);
  point -= lead;
  digits = digits.replace(/0+$/, '');
  if (digits === '') return { digits: '0', point: 1 };
  return { digits, point };
}
function incDigits(s) {
  const a = s.split('');
  let i = a.length - 1;
  while (i >= 0) {
    if (a[i] === '9') { a[i] = '0'; i--; } else { a[i] = String.fromCharCode(a[i].charCodeAt(0) + 1); return { s: a.join(''), carry: false }; }
  }
  return { s: '1' + a.join(''), carry: true };
}
// Keep `keep` leading digits, rounding the rest half away from zero.
function roundTo(d, keep) {
  if (keep >= d.digits.length) return d;
  if (keep < 0) return { digits: '0', point: 1 };
  const up = d.digits.charCodeAt(keep) >= 53;
  let kept = d.digits.slice(0, keep);
  let point = d.point;
  if (up) {
    if (kept === '') { kept = '1'; point++; } else {
      const r = incDigits(kept);
      kept = r.s;
      if (r.carry) point++;
    }
  }
  if (kept === '') return { digits: '0', point: 1 };
  kept = kept.replace(/0+$/, '');
  return kept === '' ? { digits: '0', point: 1 } : { digits: kept, point };
}
// The integer and fraction texts with at least minFD and at most maxFD fraction digits.
function fixedParts(d, minFD, maxFD) {
  const r = roundTo(d, d.point + maxFD);
  let int, frac;
  if (r.digits === '0') { int = '0'; frac = ''; } else if (r.point <= 0) { int = '0'; frac = '0'.repeat(-r.point) + r.digits; } else if (r.point >= r.digits.length) { int = r.digits + '0'.repeat(r.point - r.digits.length); frac = ''; } else { int = r.digits.slice(0, r.point); frac = r.digits.slice(r.point); }
  while (frac.length < minFD) frac += '0';
  return { int, frac, zero: r.digits === '0' };
}
function sigParts(d, minSD, maxSD) {
  const r = roundTo(d, maxSD);
  const p = fixedParts(r, 0, 1000);
  // pad to minSD significant digits
  const have = (p.int === '0' ? 0 : p.int.length) + (p.int === '0' ? p.frac.replace(/^0+/, '').length : p.frac.length);
  if (r.digits === '0') {
    p.frac = minSD > 1 ? '0'.repeat(minSD - 1) : '';
    return p;
  }
  if (have < minSD) p.frac += '0'.repeat(minSD - have);
  return p;
}
function group(int, mode) {
  if (mode === false) return [int];
  if (mode === 'min2' && int.length < 5) return [int];
  const out = [];
  let i = int.length % 3 || 3;
  out.push(int.slice(0, i));
  for (; i < int.length; i += 3) out.push(int.slice(i, i + 3));
  return out;
}

// ---- NumberFormat ----

const CURRENCIES = {
  USD: ['$', 2, 'US dollars'], EUR: ['€', 2, 'euros'], GBP: ['£', 2, 'British pounds'], JPY: ['¥', 0, 'Japanese yen'], CNY: ['CN¥', 2, 'Chinese yuan'],
  INR: ['₹', 2, 'Indian rupees'], KRW: ['₩', 0, 'South Korean won'], CAD: ['CA$', 2, 'Canadian dollars'], AUD: ['A$', 2, 'Australian dollars'], BRL: ['R$', 2, 'Brazilian reals'],
  MXN: ['MX$', 2, 'Mexican pesos'], CHF: ['CHF', 2, 'Swiss francs'], RUB: ['RUB', 2, 'Russian rubles'], SEK: ['SEK', 2, 'Swedish kronor'], NOK: ['NOK', 2, 'Norwegian kroner'],
  DKK: ['DKK', 2, 'Danish kroner'], PLN: ['PLN', 2, 'Polish zlotys'], NZD: ['NZ$', 2, 'New Zealand dollars'], HKD: ['HK$', 2, 'Hong Kong dollars'], SGD: ['SGD', 2, 'Singapore dollars'],
  ILS: ['₪', 2, 'Israeli new shekels'], VND: ['₫', 0, 'Vietnamese dong'], TWD: ['NT$', 2, 'New Taiwan dollars'], PHP: ['₱', 2, 'Philippine pesos'], BHD: ['BHD', 3, 'Bahraini dinars'],
  KWD: ['KWD', 3, 'Kuwaiti dinars'], CLP: ['CLP', 0, 'Chilean pesos'], ISK: ['ISK', 0, 'Icelandic krónur'], TRY: ['TRY', 2, 'Turkish Lira'], ZAR: ['ZAR', 2, 'South African rand'],
  THB: ['THB', 2, 'Thai baht'], CZK: ['CZK', 2, 'Czech korunas'], HUF: ['HUF', 2, 'Hungarian forints'], AED: ['AED', 2, 'United Arab Emirates dirhams'],
};
const UNITS = {
  acre: ['ac', 'acre', 'acres'], bit: ['bit', 'bit', 'bits'], byte: ['byte', 'byte', 'bytes'], celsius: ['°C', 'degree Celsius', 'degrees Celsius'],
  centimeter: ['cm', 'centimeter', 'centimeters'], day: ['day', 'day', 'days'], degree: ['deg', 'degree', 'degrees'], fahrenheit: ['°F', 'degree Fahrenheit', 'degrees Fahrenheit'],
  'fluid-ounce': ['fl oz', 'fluid ounce', 'fluid ounces'], foot: ['ft', 'foot', 'feet'], gallon: ['gal', 'gallon', 'gallons'], gigabit: ['Gb', 'gigabit', 'gigabits'],
  gigabyte: ['GB', 'gigabyte', 'gigabytes'], gram: ['g', 'gram', 'grams'], hectare: ['ha', 'hectare', 'hectares'], hour: ['hr', 'hour', 'hours'], inch: ['in', 'inch', 'inches'],
  kilobit: ['kb', 'kilobit', 'kilobits'], kilobyte: ['kB', 'kilobyte', 'kilobytes'], kilogram: ['kg', 'kilogram', 'kilograms'], kilometer: ['km', 'kilometer', 'kilometers'],
  liter: ['L', 'liter', 'liters'], megabit: ['Mb', 'megabit', 'megabits'], megabyte: ['MB', 'megabyte', 'megabytes'], meter: ['m', 'meter', 'meters'], mile: ['mi', 'mile', 'miles'],
  'mile-scandinavian': ['smi', 'mile-scandinavian', 'miles-scandinavian'], milliliter: ['mL', 'milliliter', 'milliliters'], millimeter: ['mm', 'millimeter', 'millimeters'],
  millisecond: ['ms', 'millisecond', 'milliseconds'], minute: ['min', 'minute', 'minutes'], month: ['mth', 'month', 'months'], ounce: ['oz', 'ounce', 'ounces'],
  percent: ['%', 'percent', 'percent'], petabyte: ['PB', 'petabyte', 'petabytes'], pound: ['lb', 'pound', 'pounds'], second: ['sec', 'second', 'seconds'], stone: ['st', 'stone', 'stone'],
  terabit: ['Tb', 'terabit', 'terabits'], terabyte: ['TB', 'terabyte', 'terabytes'], week: ['wk', 'week', 'weeks'], yard: ['yd', 'yard', 'yards'], year: ['yr', 'year', 'years'],
};
const COMPACT = [[12, 'T', ' trillion'], [9, 'B', ' billion'], [6, 'M', ' million'], [3, 'K', ' thousand']];

class NumberFormat {
  constructor(locales, options) {
    const loc = resolveLocale(locales);
    const o = getOpts(options);
    const s = { brand: 'NumberFormat', locale: loc, numberingSystem: 'latn' };
    s.style = optStr(o, 'style', ['decimal', 'percent', 'currency', 'unit'], 'decimal');
    let cur = o.currency;
    if (cur !== undefined) {
      cur = String(cur);
      if (!/^[A-Za-z]{3}$/.test(cur)) throw new RangeError('Invalid currency code : ' + cur);
      cur = cur.toUpperCase();
    }
    if (s.style === 'currency' && cur === undefined) throw new TypeError('Currency code is required with currency style.');
    s.currency = s.style === 'currency' ? cur : undefined;
    s.currencyDisplay = optStr(o, 'currencyDisplay', ['code', 'symbol', 'narrowSymbol', 'name'], 'symbol');
    s.currencySign = optStr(o, 'currencySign', ['standard', 'accounting'], 'standard');
    let unit = o.unit;
    if (unit !== undefined) {
      unit = String(unit);
      if (!hop.call(UNITS, unit) && !/^[a-z-]+-per-[a-z-]+$/.test(unit)) throw new RangeError('Invalid unit argument for Intl.NumberFormat() \'' + unit + '\'');
    }
    if (s.style === 'unit' && unit === undefined) throw new TypeError('Unit is required with unit style.');
    s.unit = s.style === 'unit' ? unit : undefined;
    s.unitDisplay = optStr(o, 'unitDisplay', ['short', 'narrow', 'long'], 'short');
    s.notation = optStr(o, 'notation', ['standard', 'scientific', 'engineering', 'compact'], 'standard');
    s.compactDisplay = optStr(o, 'compactDisplay', ['short', 'long'], 'short');
    const cd = s.style === 'currency' ? (hop.call(CURRENCIES, s.currency) ? CURRENCIES[s.currency][1] : 2) : 0;
    s.minimumIntegerDigits = optNum(o, 'minimumIntegerDigits', 1, 21, 1);
    const hasSD = o.minimumSignificantDigits !== undefined || o.maximumSignificantDigits !== undefined;
    const hasFD = o.minimumFractionDigits !== undefined || o.maximumFractionDigits !== undefined;
    if (hasSD) {
      s.minimumSignificantDigits = optNum(o, 'minimumSignificantDigits', 1, 21, 1);
      s.maximumSignificantDigits = optNum(o, 'maximumSignificantDigits', s.minimumSignificantDigits, 21, 21);
    }
    if (hasFD || !hasSD) {
      const dmin = s.style === 'currency' ? cd : 0;
      const dmax = s.style === 'currency' ? cd : s.style === 'percent' ? 0 : 3;
      let mn = optNum(o, 'minimumFractionDigits', 0, 100, undefined);
      let mx = optNum(o, 'maximumFractionDigits', 0, 100, undefined);
      if (mn === undefined && mx === undefined) { mn = dmin; mx = dmax; } else if (mn === undefined) { mn = Math.min(dmin, mx); } else if (mx === undefined) { mx = Math.max(dmax, mn); } else if (mn > mx) throw new RangeError('maximumFractionDigits value is out of range.');
      s.minimumFractionDigits = mn;
      s.maximumFractionDigits = mx;
      s.defaultFraction = !hasFD;
    }
    let ug = o.useGrouping;
    s.useGrouping = ug === undefined ? (s.notation === 'compact' ? 'min2' : 'auto') : ug === false || ug === 'false' ? false : ug === 'min2' ? 'min2' : ug === 'always' ? 'always' : 'auto';
    s.signDisplay = optStr(o, 'signDisplay', ['auto', 'never', 'always', 'exceptZero', 'negative'], 'auto');
    s.roundingMode = optStr(o, 'roundingMode', ['ceil', 'floor', 'expand', 'trunc', 'halfCeil', 'halfFloor', 'halfExpand', 'halfTrunc', 'halfEven'], 'halfExpand');
    setSlots(this, s);
  }
  get format() {
    const s = slots(this, 'NumberFormat');
    if (!s.bound) s.bound = (v) => formatNumberToParts(s, v).map((p) => p.value).join('');
    return s.bound;
  }
  formatToParts(v) {
    return formatNumberToParts(slots(this, 'NumberFormat'), v);
  }
  formatRange(a, b) {
    const s = slots(this, 'NumberFormat');
    const x = this.format(a), y = this.format(b);
    return x === y ? '~' + x : x + '–' + y;
  }
  resolvedOptions() {
    const s = slots(this, 'NumberFormat');
    const o = { locale: s.locale, numberingSystem: 'latn', style: s.style };
    if (s.style === 'currency') { o.currency = s.currency; o.currencyDisplay = s.currencyDisplay; o.currencySign = s.currencySign; }
    if (s.style === 'unit') { o.unit = s.unit; o.unitDisplay = s.unitDisplay; }
    o.minimumIntegerDigits = s.minimumIntegerDigits;
    if (s.minimumFractionDigits !== undefined) { o.minimumFractionDigits = s.minimumFractionDigits; o.maximumFractionDigits = s.maximumFractionDigits; }
    if (s.minimumSignificantDigits !== undefined) { o.minimumSignificantDigits = s.minimumSignificantDigits; o.maximumSignificantDigits = s.maximumSignificantDigits; }
    o.useGrouping = s.useGrouping;
    o.notation = s.notation;
    if (s.notation === 'compact') o.compactDisplay = s.compactDisplay;
    o.signDisplay = s.signDisplay;
    o.roundingIncrement = 1;
    o.roundingMode = s.roundingMode;
    o.roundingPriority = 'auto';
    o.trailingZeroDisplay = 'auto';
    return o;
  }
  static supportedLocalesOf(l) { return canonLocales(l); }
}
tag(NumberFormat, 'Intl.NumberFormat');
enumerable(NumberFormat, ['format', 'formatToParts', 'formatRange', 'resolvedOptions']);

function numberToDigits(s, x) {
  // -> { int, frac, exp? } for |x| in the format's notation
  let d = decimalOf(x);
  const out = {};
  if (s.notation === 'compact') {
    let bi = COMPACT.findIndex((c) => d.point - 1 >= c[0]);
    for (;;) {
      const pow = bi >= 0 ? COMPACT[bi][0] : 0;
      const suffix = bi >= 0 ? (s.compactDisplay === 'long' ? COMPACT[bi][2] : COMPACT[bi][1]) : '';
      const dd = { digits: d.digits, point: d.point - pow };
      let p;
      if (s.minimumSignificantDigits !== undefined) p = sigParts(dd, s.minimumSignificantDigits, s.maximumSignificantDigits);
      else if (!s.defaultFraction) p = fixedParts(dd, s.minimumFractionDigits, s.maximumFractionDigits);
      else {
        const ints = dd.digits === '0' ? 1 : Math.max(dd.point, 1);
        p = fixedParts(dd, 0, Math.max(0, 2 - ints));
      }
      // 999999 rounds to 1000K: that is 1M
      if (p.int.length > 3 && bi !== 0) {
        bi = bi < 0 ? COMPACT.length - 1 : bi - 1;
        continue;
      }
      out.int = p.int; out.frac = p.frac; out.compact = suffix; out.zero = p.zero;
      return out;
    }
  }
  if (s.notation === 'scientific' || s.notation === 'engineering') {
    let e = d.point - 1;
    if (s.notation === 'engineering') e = Math.floor(e / 3) * 3;
    d = { digits: d.digits, point: d.point - e };
    let p;
    if (s.minimumSignificantDigits !== undefined) p = sigParts(d, s.minimumSignificantDigits, s.maximumSignificantDigits);
    else p = fixedParts(d, s.minimumFractionDigits, s.maximumFractionDigits);
    out.int = p.int; out.frac = p.frac; out.exp = e; out.zero = p.zero;
    return out;
  }
  let p;
  if (s.minimumSignificantDigits !== undefined && (s.minimumFractionDigits === undefined || !s.defaultFraction)) p = sigParts(d, s.minimumSignificantDigits, s.maximumSignificantDigits);
  else if (s.minimumSignificantDigits !== undefined) p = sigParts(d, s.minimumSignificantDigits, s.maximumSignificantDigits);
  else p = fixedParts(d, s.minimumFractionDigits, s.maximumFractionDigits);
  out.int = p.int; out.frac = p.frac; out.zero = p.zero;
  return out;
}

function formatNumberToParts(s, v) {
  let x = typeof v === 'bigint' ? Number(v) : Number(v);
  const parts = [];
  const lit = (t) => parts.push({ type: 'literal', value: t });
  if (x !== x) {
    parts.push({ type: 'nan', value: 'NaN' });
    return decorate(s, parts, false, true);
  }
  const neg = x < 0 || (x === 0 && 1 / x < 0);
  x = Math.abs(x);
  if (s.style === 'percent') x *= 100;
  if (x === Infinity) {
    parts.push({ type: 'infinity', value: '∞' });
    return decorate(s, parts, neg, false);
  }
  const d = numberToDigits(s, x);
  let int = d.int;
  while (int.length < s.minimumIntegerDigits) int = '0' + int;
  const gm = s.useGrouping === 'always' ? true : s.useGrouping === false ? false : s.useGrouping === 'min2' ? 'min2' : true;
  const g = group(int, gm);
  g.forEach((chunk, i) => {
    if (i) parts.push({ type: 'group', value: ',' });
    parts.push({ type: 'integer', value: chunk });
  });
  if (d.frac) {
    parts.push({ type: 'decimal', value: '.' });
    parts.push({ type: 'fraction', value: d.frac });
  }
  if (d.exp !== undefined) {
    parts.push({ type: 'exponentSeparator', value: 'E' });
    if (d.exp < 0) parts.push({ type: 'exponentMinusSign', value: '-' });
    parts.push({ type: 'exponentInteger', value: String(Math.abs(d.exp)) });
  }
  if (d.compact) {
    if (d.compact.charAt(0) === ' ') lit(' ');
    parts.push({ type: 'compact', value: d.compact.trim() });
  }
  return decorate(s, parts, neg, d.zero);
}

function decorate(s, parts, neg, isZero) {
  // sign
  let sign = '';
  const sd = s.signDisplay;
  if (sd === 'auto') sign = neg ? '-' : '';
  else if (sd === 'always') sign = neg ? '-' : '+';
  else if (sd === 'exceptZero') sign = isZero ? '' : neg ? '-' : '+';
  else if (sd === 'negative') sign = neg && !isZero ? '-' : '';
  else sign = '';
  const out = [];
  const signPart = (acc) => {
    if (!sign) return;
    if (acc && sign === '-') { out.unshift({ type: 'literal', value: '(' }); return; }
    out.unshift({ type: sign === '-' ? 'minusSign' : 'plusSign', value: sign });
  };
  if (s.style === 'currency') {
    const info = hop.call(CURRENCIES, s.currency) ? CURRENCIES[s.currency] : null;
    let sym = s.currencyDisplay === 'code' ? s.currency : info ? info[0] : s.currency;
    if (s.currencyDisplay === 'narrowSymbol' && info) sym = info[0].replace(/^[A-Z]{1,3}\$$/, '$').replace(/^CN¥$/, '¥');
    if (s.currencyDisplay === 'name') {
      const name = info ? info[2] : s.currency;
      for (const p of parts) out.push(p);
      out.push({ type: 'literal', value: ' ' });
      out.push({ type: 'currency', value: name });
      signPart(false);
      return out;
    }
    const acc = s.currencySign === 'accounting' && sign === '-';
    out.push({ type: 'currency', value: sym });
    if (/^[A-Za-z]+$/.test(sym)) out.push({ type: 'literal', value: ' ' });
    for (const p of parts) out.push(p);
    if (acc) {
      out.unshift({ type: 'literal', value: '(' });
      out.push({ type: 'literal', value: ')' });
    } else signPart(false);
    return out;
  }
  for (const p of parts) out.push(p);
  if (s.style === 'percent') out.push({ type: 'percentSign', value: '%' });
  if (s.style === 'unit') {
    const parsed = unitParts(s.unit);
    const u = parsed;
    const one = out.length === 1 && out[0].value === '1';
    if (s.unitDisplay === 'long') {
      out.push({ type: 'literal', value: ' ' });
      out.push({ type: 'unit', value: u.long(one) });
    } else if (s.unitDisplay === 'narrow') {
      const sp = u.narrowSpace;
      if (sp) out.push({ type: 'literal', value: ' ' });
      out.push({ type: 'unit', value: u.short });
    } else {
      if (u.short !== '%' && !/^°/.test(u.short)) out.push({ type: 'literal', value: ' ' });
      out.push({ type: 'unit', value: u.short + (u.plural && !one ? 's' : '') });
    }
  }
  signPart(false);
  return out;
}
const COMPOUND_UNITS = {
  'kilometer-per-hour': ['km/h', 'kilometer per hour', 'kilometers per hour'],
  'mile-per-hour': ['mph', 'mile per hour', 'miles per hour'],
  'meter-per-second': ['m/s', 'meter per second', 'meters per second'],
};
function unitParts(unit) {
  if (hop.call(COMPOUND_UNITS, unit)) {
    const u = COMPOUND_UNITS[unit];
    return { short: u[0], long: (one) => (one ? u[1] : u[2]), narrowSpace: false, plural: false };
  }
  const per = /^(.+)-per-(.+)$/.exec(unit);
  if (per && hop.call(UNITS, per[1]) && hop.call(UNITS, per[2])) {
    const a = UNITS[per[1]], b = UNITS[per[2]];
    return { short: a[0] + '/' + b[0], long: (one) => (one ? a[1] : a[2]) + ' per ' + b[1], narrowSpace: false, plural: false };
  }
  if (!hop.call(UNITS, unit)) return { short: unit, long: () => unit, narrowSpace: true, plural: false };
  const u = UNITS[unit];
  const plural = unit === 'day' || unit === 'week' || unit === 'month' || unit === 'year';
  return { short: u[0], long: (one) => (one ? u[1] : u[2]), narrowSpace: false, plural };
}

// ---- time zones ----

const ZONES = {
  UTC: [0, null, 'UTC', 'UTC', 'Coordinated Universal Time', 'Coordinated Universal Time'],
  'America/New_York': [-300, 'US', 'EST', 'EDT', 'Eastern Standard Time', 'Eastern Daylight Time'],
  'America/Detroit': [-300, 'US', 'EST', 'EDT', 'Eastern Standard Time', 'Eastern Daylight Time'],
  'America/Toronto': [-300, 'US', 'EST', 'EDT', 'Eastern Standard Time', 'Eastern Daylight Time'],
  'America/Chicago': [-360, 'US', 'CST', 'CDT', 'Central Standard Time', 'Central Daylight Time'],
  'America/Mexico_City': [-360, null, 'CST', 'CST', 'Central Standard Time', 'Central Standard Time'],
  'America/Denver': [-420, 'US', 'MST', 'MDT', 'Mountain Standard Time', 'Mountain Daylight Time'],
  'America/Phoenix': [-420, null, 'MST', 'MST', 'Mountain Standard Time', 'Mountain Standard Time'],
  'America/Los_Angeles': [-480, 'US', 'PST', 'PDT', 'Pacific Standard Time', 'Pacific Daylight Time'],
  'America/Vancouver': [-480, 'US', 'PST', 'PDT', 'Pacific Standard Time', 'Pacific Daylight Time'],
  'America/Anchorage': [-540, 'US', 'AKST', 'AKDT', 'Alaska Standard Time', 'Alaska Daylight Time'],
  'Pacific/Honolulu': [-600, null, 'HST', 'HST', 'Hawaii-Aleutian Standard Time', 'Hawaii-Aleutian Standard Time'],
  'America/Sao_Paulo': [-180, null, 'GMT-3', 'GMT-3', 'Brasilia Standard Time', 'Brasilia Standard Time'],
  'America/Argentina/Buenos_Aires': [-180, null, 'GMT-3', 'GMT-3', 'Argentina Standard Time', 'Argentina Standard Time'],
  'Europe/London': [0, 'EU', 'GMT', 'GMT+1', 'Greenwich Mean Time', 'British Summer Time'],
  'Europe/Dublin': [0, 'EU', 'GMT', 'GMT+1', 'Greenwich Mean Time', 'Irish Standard Time'],
  'Europe/Lisbon': [0, 'EU', 'GMT', 'GMT+1', 'Western European Standard Time', 'Western European Summer Time'],
  'Europe/Paris': [60, 'EU', 'GMT+1', 'GMT+2', 'Central European Standard Time', 'Central European Summer Time'],
  'Europe/Berlin': [60, 'EU', 'GMT+1', 'GMT+2', 'Central European Standard Time', 'Central European Summer Time'],
  'Europe/Madrid': [60, 'EU', 'GMT+1', 'GMT+2', 'Central European Standard Time', 'Central European Summer Time'],
  'Europe/Rome': [60, 'EU', 'GMT+1', 'GMT+2', 'Central European Standard Time', 'Central European Summer Time'],
  'Europe/Amsterdam': [60, 'EU', 'GMT+1', 'GMT+2', 'Central European Standard Time', 'Central European Summer Time'],
  'Europe/Zurich': [60, 'EU', 'GMT+1', 'GMT+2', 'Central European Standard Time', 'Central European Summer Time'],
  'Europe/Stockholm': [60, 'EU', 'GMT+1', 'GMT+2', 'Central European Standard Time', 'Central European Summer Time'],
  'Europe/Vienna': [60, 'EU', 'GMT+1', 'GMT+2', 'Central European Standard Time', 'Central European Summer Time'],
  'Europe/Warsaw': [60, 'EU', 'GMT+1', 'GMT+2', 'Central European Standard Time', 'Central European Summer Time'],
  'Europe/Athens': [120, 'EU', 'GMT+2', 'GMT+3', 'Eastern European Standard Time', 'Eastern European Summer Time'],
  'Europe/Helsinki': [120, 'EU', 'GMT+2', 'GMT+3', 'Eastern European Standard Time', 'Eastern European Summer Time'],
  'Europe/Kiev': [120, 'EU', 'GMT+2', 'GMT+3', 'Eastern European Standard Time', 'Eastern European Summer Time'],
  'Europe/Istanbul': [180, null, 'GMT+3', 'GMT+3', 'GMT+03:00', 'GMT+03:00'],
  'Europe/Moscow': [180, null, 'GMT+3', 'GMT+3', 'Moscow Standard Time', 'Moscow Standard Time'],
  'Africa/Cairo': [120, null, 'GMT+2', 'GMT+2', 'Eastern European Standard Time', 'Eastern European Standard Time'],
  'Africa/Johannesburg': [120, null, 'GMT+2', 'GMT+2', 'South Africa Standard Time', 'South Africa Standard Time'],
  'Africa/Lagos': [60, null, 'GMT+1', 'GMT+1', 'West Africa Standard Time', 'West Africa Standard Time'],
  'Africa/Nairobi': [180, null, 'GMT+3', 'GMT+3', 'East Africa Time', 'East Africa Time'],
  'Asia/Dubai': [240, null, 'GMT+4', 'GMT+4', 'Gulf Standard Time', 'Gulf Standard Time'],
  'Asia/Karachi': [300, null, 'GMT+5', 'GMT+5', 'Pakistan Standard Time', 'Pakistan Standard Time'],
  'Asia/Kolkata': [330, null, 'GMT+5:30', 'GMT+5:30', 'India Standard Time', 'India Standard Time'],
  'Asia/Calcutta': [330, null, 'GMT+5:30', 'GMT+5:30', 'India Standard Time', 'India Standard Time'],
  'Asia/Dhaka': [360, null, 'GMT+6', 'GMT+6', 'Bangladesh Standard Time', 'Bangladesh Standard Time'],
  'Asia/Bangkok': [420, null, 'GMT+7', 'GMT+7', 'Indochina Time', 'Indochina Time'],
  'Asia/Jakarta': [420, null, 'GMT+7', 'GMT+7', 'Western Indonesia Time', 'Western Indonesia Time'],
  'Asia/Shanghai': [480, null, 'GMT+8', 'GMT+8', 'China Standard Time', 'China Standard Time'],
  'Asia/Hong_Kong': [480, null, 'GMT+8', 'GMT+8', 'Hong Kong Standard Time', 'Hong Kong Standard Time'],
  'Asia/Singapore': [480, null, 'GMT+8', 'GMT+8', 'Singapore Standard Time', 'Singapore Standard Time'],
  'Asia/Taipei': [480, null, 'GMT+8', 'GMT+8', 'Taipei Standard Time', 'Taipei Standard Time'],
  'Australia/Perth': [480, null, 'GMT+8', 'GMT+8', 'Australian Western Standard Time', 'Australian Western Standard Time'],
  'Asia/Tokyo': [540, null, 'GMT+9', 'GMT+9', 'Japan Standard Time', 'Japan Standard Time'],
  'Asia/Seoul': [540, null, 'GMT+9', 'GMT+9', 'Korean Standard Time', 'Korean Standard Time'],
  'Australia/Brisbane': [600, null, 'GMT+10', 'GMT+10', 'Australian Eastern Standard Time', 'Australian Eastern Standard Time'],
  'Australia/Sydney': [600, 'AU', 'GMT+10', 'GMT+11', 'Australian Eastern Standard Time', 'Australian Eastern Daylight Time'],
  'Australia/Melbourne': [600, 'AU', 'GMT+10', 'GMT+11', 'Australian Eastern Standard Time', 'Australian Eastern Daylight Time'],
  'Pacific/Auckland': [720, 'NZ', 'GMT+12', 'GMT+13', 'New Zealand Standard Time', 'New Zealand Daylight Time'],
};
const ZONE_ALIAS = { GMT: 'UTC', 'Etc/UTC': 'UTC', 'Etc/GMT': 'UTC', 'Etc/Universal': 'UTC', Zulu: 'UTC', 'US/Eastern': 'America/New_York', 'US/Central': 'America/Chicago', 'US/Mountain': 'America/Denver', 'US/Pacific': 'America/Los_Angeles', 'Asia/Saigon': 'Asia/Bangkok' };

function dayOfWeek(y, m, d) {
  return new Date(Date.UTC(y, m, d)).getUTCDay();
}
// The date (UTC ms) of the n-th (1-based; 0 = last) Sunday of month m (0-based) in year y at hh:00 UTC.
function sundayOf(y, m, n, hourUtc) {
  if (n === 0) {
    const last = new Date(Date.UTC(y, m + 1, 0)).getUTCDate();
    const dow = dayOfWeek(y, m, last);
    return Date.UTC(y, m, last - dow, hourUtc);
  }
  const dow1 = dayOfWeek(y, m, 1);
  const first = 1 + ((7 - dow1) % 7);
  return Date.UTC(y, m, first + (n - 1) * 7, hourUtc);
}
function isDst(rule, std, ms) {
  if (!rule) return false;
  const y = new Date(ms).getUTCFullYear();
  if (rule === 'US') {
    // second Sunday of March 2:00 local standard time -> first Sunday of November 2:00 local daylight
    const start = sundayOf(y, 2, 2, 2) - std * 60000;
    const end = sundayOf(y, 10, 1, 2) - (std + 60) * 60000;
    return ms >= start && ms < end;
  }
  if (rule === 'EU') {
    const start = sundayOf(y, 2, 0, 1);
    const end = sundayOf(y, 9, 0, 1);
    return ms >= start && ms < end;
  }
  if (rule === 'AU') {
    // first Sunday of April 3:00 local daylight -> first Sunday of October 2:00 local standard
    const end = sundayOf(y, 3, 1, 3) - (std + 60) * 60000;
    const start = sundayOf(y, 9, 1, 2) - std * 60000;
    return ms < end || ms >= start;
  }
  if (rule === 'NZ') {
    const end = sundayOf(y, 3, 1, 3) - (std + 60) * 60000;
    const start = sundayOf(y, 8, 0, 2) - std * 60000;
    return ms < end || ms >= start;
  }
  return false;
}
function resolveZone(tz) {
  if (tz === undefined) {
    const off = -new Date(0).getTimezoneOffset();
    return { kind: 'local', name: off === 0 ? 'UTC' : 'Etc/GMT' + (off < 0 ? '+' : '-') + Math.abs(off / 60), off };
  }
  tz = String(tz);
  let name = tz;
  const m = /^([+-])(\d{2}):?(\d{2})?$/.exec(tz);
  if (m) {
    const off = (m[1] === '-' ? -1 : 1) * (parseInt(m[2], 10) * 60 + (m[3] ? parseInt(m[3], 10) : 0));
    return { kind: 'fixed', name: tz, off };
  }
  const e = /^Etc\/GMT([+-])(\d{1,2})$/.exec(tz);
  if (e) return { kind: 'fixed', name: tz, off: (e[1] === '+' ? -1 : 1) * parseInt(e[2], 10) * 60 };
  for (const k of Object.keys(ZONES)) if (k.toLowerCase() === tz.toLowerCase()) name = k;
  for (const k of Object.keys(ZONE_ALIAS)) if (k.toLowerCase() === tz.toLowerCase()) name = ZONE_ALIAS[k];
  if (!hop.call(ZONES, name)) throw new RangeError('Invalid time zone specified: ' + tz);
  return { kind: 'zone', name, z: ZONES[name] };
}
function zoneOffset(zone, ms) {
  if (zone.kind === 'local') return -new Date(ms).getTimezoneOffset();
  if (zone.kind === 'fixed') return zone.off;
  const z = zone.z;
  return z[0] + (isDst(z[1], z[0], ms) ? 60 : 0);
}
function zoneName(zone, ms, style) {
  const off = zoneOffset(zone, ms);
  const gmt = (long) => {
    if (off === 0) return 'GMT';
    const a = Math.abs(off), h = Math.floor(a / 60), mi = a % 60;
    if (long) return 'GMT' + (off < 0 ? '-' : '+') + (h < 10 ? '0' : '') + h + ':' + (mi < 10 ? '0' : '') + mi;
    return 'GMT' + (off < 0 ? '-' : '+') + h + (mi ? ':' + (mi < 10 ? '0' : '') + mi : '');
  };
  if (style === 'shortOffset') return gmt(false);
  if (style === 'longOffset') return gmt(true);
  if (zone.kind === 'zone') {
    const z = zone.z;
    const dst = isDst(z[1], z[0], ms);
    if (style === 'long') return dst ? z[5] : z[4];
    return dst ? z[3] : z[2];
  }
  if (zone.kind === 'local' && off === 0) return style === 'long' ? 'Coordinated Universal Time' : 'UTC';
  return style === 'long' ? gmt(true) : gmt(false);
}

// ---- DateTimeFormat ----

const MONTHS = ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December'];
const DAYS = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];
const pad2 = (n) => (n < 10 ? '0' : '') + n;

function timeClip(v) {
  if (v === undefined) return Date.now();
  if (hasBrand(v, 'Temporal')) return v;
  const t = v instanceof Date ? v.getTime() : Number(v);
  return t;
}

class DateTimeFormat {
  constructor(locales, options) {
    const loc = resolveLocale(locales);
    const o = getOpts(options);
    const s = { brand: 'DateTimeFormat', locale: loc, calendar: 'gregory', numberingSystem: 'latn' };
    s.zone = resolveZone(o.timeZone);
    s.hour12 = o.hour12 === undefined ? undefined : Boolean(o.hour12);
    s.hourCycle = optStr(o, 'hourCycle', ['h11', 'h12', 'h23', 'h24'], undefined);
    s.weekday = optStr(o, 'weekday', ['narrow', 'short', 'long'], undefined);
    s.era = optStr(o, 'era', ['narrow', 'short', 'long'], undefined);
    s.year = optStr(o, 'year', ['2-digit', 'numeric'], undefined);
    s.month = optStr(o, 'month', ['2-digit', 'numeric', 'narrow', 'short', 'long'], undefined);
    s.day = optStr(o, 'day', ['2-digit', 'numeric'], undefined);
    s.dayPeriod = optStr(o, 'dayPeriod', ['narrow', 'short', 'long'], undefined);
    s.hour = optStr(o, 'hour', ['2-digit', 'numeric'], undefined);
    s.minute = optStr(o, 'minute', ['2-digit', 'numeric'], undefined);
    s.second = optStr(o, 'second', ['2-digit', 'numeric'], undefined);
    s.fractionalSecondDigits = optNum(o, 'fractionalSecondDigits', 1, 3, undefined);
    s.timeZoneName = optStr(o, 'timeZoneName', ['short', 'long', 'shortOffset', 'longOffset', 'shortGeneric', 'longGeneric'], undefined);
    s.dateStyle = optStr(o, 'dateStyle', ['full', 'long', 'medium', 'short'], undefined);
    s.timeStyle = optStr(o, 'timeStyle', ['full', 'long', 'medium', 'short'], undefined);
    const anyComp = s.weekday || s.era || s.year || s.month || s.day || s.dayPeriod || s.hour || s.minute || s.second || s.fractionalSecondDigits || s.timeZoneName;
    if ((s.dateStyle || s.timeStyle) && anyComp) throw new TypeError("Can't set option " + ['weekday', 'era', 'year', 'month', 'day', 'dayPeriod', 'hour', 'minute', 'second', 'fractionalSecondDigits', 'timeZoneName'].find((k) => s[k] !== undefined) + ' when ' + (s.dateStyle ? 'dateStyle' : 'timeStyle') + ' is used');
    // Without any date or time field (or style) there are defaults: the date; toLocaleString and
    // its relatives say which they want in __defaults ('none': the options had what they need).
    const defaults = o.__defaults === undefined ? 'date' : o.__defaults;
    const anyField = s.weekday || s.year || s.month || s.day || s.dayPeriod || s.hour || s.minute || s.second || s.fractionalSecondDigits;
    if (!s.dateStyle && !s.timeStyle && (o.__defaults !== undefined ? defaults !== 'none' : !anyField)) {
      if (defaults === 'date' || defaults === 'all') { s.year = 'numeric'; s.month = 'numeric'; s.day = 'numeric'; }
      if (defaults === 'time' || defaults === 'all') { s.hour = 'numeric'; s.minute = 'numeric'; s.second = 'numeric'; }
    }
    // The hour cycle.
    let hc = s.hourCycle;
    if (s.hour12 !== undefined) hc = s.hour12 ? 'h12' : 'h23';
    if (!hc) hc = 'h12';
    s.hc = hc;
    setSlots(this, s);
  }
  get format() {
    const s = slots(this, 'DateTimeFormat');
    if (!s.bound) s.bound = (d) => dtfParts(s, d).map((p) => p.value).join('');
    return s.bound;
  }
  formatToParts(d) {
    return dtfParts(slots(this, 'DateTimeFormat'), d);
  }
  formatRange(a, b) {
    const x = this.format(a), y = this.format(b);
    return x === y ? x : x + ' – ' + y;
  }
  formatRangeToParts(a, b) {
    const s = slots(this, 'DateTimeFormat');
    return dtfParts(s, a).map((p) => Object.assign({ source: 'startRange' }, p)).concat([{ type: 'literal', value: ' – ', source: 'shared' }], dtfParts(s, b).map((p) => Object.assign({ source: 'endRange' }, p)));
  }
  resolvedOptions() {
    const s = slots(this, 'DateTimeFormat');
    const o = { locale: s.locale, calendar: 'gregory', numberingSystem: 'latn', timeZone: s.zone.name };
    if (s.hour || s.timeStyle) { o.hourCycle = s.hc; o.hour12 = s.hc === 'h12' || s.hc === 'h11'; }
    for (const k of ['weekday', 'era', 'year', 'month', 'day', 'dayPeriod', 'hour', 'minute', 'second', 'fractionalSecondDigits', 'timeZoneName', 'dateStyle', 'timeStyle']) if (s[k] !== undefined) o[k] = s[k];
    return o;
  }
  static supportedLocalesOf(l) { return canonLocales(l); }
}
tag(DateTimeFormat, 'Intl.DateTimeFormat');
enumerable(DateTimeFormat, ['format', 'formatToParts', 'formatRange', 'formatRangeToParts', 'resolvedOptions']);

function fieldsOf(s, ms) {
  const off = zoneOffset(s.zone, ms);
  const d = new Date(ms + off * 60000);
  return { y: d.getUTCFullYear(), mo: d.getUTCMonth(), d: d.getUTCDate(), wd: d.getUTCDay(), h: d.getUTCHours(), mi: d.getUTCMinutes(), s: d.getUTCSeconds(), ms: d.getUTCMilliseconds() };
}

function dtfParts(s, date) {
  const t = timeClip(date);
  if (t !== t || Math.abs(t) > 8.64e15) throw new RangeError('Invalid time value');
  const f = fieldsOf(s, t);
  let y = s.year, mo = s.month, d = s.day, wd = s.weekday, h = s.hour, mi = s.minute, sec = s.second, tzn = s.timeZoneName, fsd = s.fractionalSecondDigits;
  let dateStyle = s.dateStyle, timeStyle = s.timeStyle;
  if (dateStyle) {
    if (dateStyle === 'full') { wd = 'long'; mo = 'long'; d = 'numeric'; y = 'numeric'; } else if (dateStyle === 'long') { mo = 'long'; d = 'numeric'; y = 'numeric'; } else if (dateStyle === 'medium') { mo = 'short'; d = 'numeric'; y = 'numeric'; } else { mo = 'numeric'; d = 'numeric'; y = '2-digit'; }
  }
  if (timeStyle) {
    h = 'numeric'; mi = '2-digit';
    if (timeStyle !== 'short') sec = '2-digit';
    if (timeStyle === 'full') tzn = 'long';
    else if (timeStyle === 'long') tzn = 'short';
  }
  const parts = [];
  const lit = (v) => parts.push({ type: 'literal', value: v });
  // --- the date part
  const hasDate = y || mo || d || wd;
  const textual = mo === 'long' || mo === 'short' || mo === 'narrow';
  if (hasDate) {
    const yearText = () => (y === '2-digit' ? pad2(f.y % 100) : String(f.y));
    const dayText = () => (d === '2-digit' ? pad2(f.d) : String(f.d));
    const monText = () => (mo === 'long' ? MONTHS[f.mo] : mo === 'short' ? MONTHS[f.mo].slice(0, 3) : mo === 'narrow' ? MONTHS[f.mo][0] : mo === '2-digit' ? pad2(f.mo + 1) : String(f.mo + 1));
    const wdText = () => (wd === 'long' ? DAYS[f.wd] : wd === 'short' ? DAYS[f.wd].slice(0, 3) : DAYS[f.wd][0]);
    if (textual) {
      // [weekday, ]Month [day][, year]
      if (wd) { parts.push({ type: 'weekday', value: wdText() }); if (mo || d || y) lit(', '); }
      parts.push({ type: 'month', value: monText() });
      if (d) { lit(' '); parts.push({ type: 'day', value: dayText() }); }
      if (y) { lit(d ? ', ' : ' '); parts.push({ type: 'year', value: yearText() }); }
    } else {
      // [weekday, ]M/D/YYYY (the parts that are there)
      if (wd) { parts.push({ type: 'weekday', value: wdText() }); if (mo || d || y) lit(', '); }
      const seq = [];
      if (mo) seq.push(['month', monText()]);
      if (d) seq.push(['day', dayText()]);
      if (y) seq.push(['year', yearText()]);
      seq.forEach((p, i) => { if (i) lit('/'); parts.push({ type: p[0], value: p[1] }); });
    }
  }
  // --- the time part
  const hasTime = h || mi || sec || fsd || s.dayPeriod;
  if (hasTime) {
    if (hasDate) lit(dateStyle && (dateStyle === 'full' || dateStyle === 'long') || (textual && !dateStyle && mo === 'long') ? ' at ' : ', ');
    const h12 = s.hc === 'h12' || s.hc === 'h11';
    let hv = f.h;
    let period = '';
    if (h12) {
      period = hv < 12 ? 'AM' : 'PM';
      hv = s.hc === 'h11' ? hv % 12 : hv % 12 === 0 ? 12 : hv % 12;
    } else if (s.hc === 'h24' && hv === 0) hv = 24;
    const seq = [];
    if (h) seq.push(['hour', h === '2-digit' || (!h12 && (mi || sec)) ? pad2(hv) : String(hv)]);
    if (mi) seq.push(['minute', h || sec ? pad2(f.mi) : mi === '2-digit' ? pad2(f.mi) : String(f.mi)]);
    if (sec) seq.push(['second', mi ? pad2(f.s) : sec === '2-digit' ? pad2(f.s) : String(f.s)]);
    seq.forEach((p, i) => { if (i) lit(':'); parts.push({ type: p[0], value: p[1] }); });
    if (fsd) {
      if (seq.length) lit('.');
      parts.push({ type: 'fractionalSecond', value: String(f.ms + 1000).slice(1, 1 + fsd) });
    }
    if (h && h12) { lit(' '); parts.push({ type: 'dayPeriod', value: period }); }
  }
  if (tzn) {
    if (parts.length) lit(' ');
    parts.push({ type: 'timeZoneName', value: zoneName(s.zone, t, tzn === 'shortGeneric' ? 'short' : tzn === 'longGeneric' ? 'long' : tzn) });
  }
  if (s.era) {
    lit(' ');
    parts.push({ type: 'era', value: f.y > 0 ? (s.era === 'long' ? 'Anno Domini' : s.era === 'short' ? 'AD' : 'A') : s.era === 'long' ? 'Before Christ' : s.era === 'short' ? 'BC' : 'B' });
  }
  return parts;
}

// ---- Collator ----

const FOLD_FROM = 'àáâãäåāăąçćĉċčďđèéêëēĕėęěĝğġģĥħìíîïĩīĭįıĵķĺļľłñńņňòóôõöøōŏőŕŗřśŝşšţťŧùúûüũūŭůűųŵýÿŷźżžÀÁÂÃÄÅĀĂĄÇĆĈĊČĎĐÈÉÊËĒĔĖĘĚĜĞĠĢĤĦÌÍÎÏĨĪĬĮİĴĶĹĻĽŁÑŃŅŇÒÓÔÕÖØŌŎŐŔŖŘŚŜŞŠŢŤŦÙÚÛÜŨŪŬŮŰŲŴÝŸŶŹŻŽ';
const FOLD_TO = 'aaaaaaaaacccccddeeeeeeeeegggghhiiiiiiiiijklllllnnnnoooooooooorrrssssttt' + 'uuuuuuuuuuwyyyzzz' + 'AAAAAAAAACCCCCDDEEEEEEEEEGGGGHHIIIIIIIIIJKLLLLLNNNNOOOOOOOOOORRRSSSSTTTUUUUUUUUUUWYYYZZZ';
const foldMap = {};
for (let i = 0; i < FOLD_FROM.length; i++) foldMap[FOLD_FROM[i]] = FOLD_TO[i];
foldMap['ß'] = 'ss'; foldMap['æ'] = 'ae'; foldMap['Æ'] = 'AE'; foldMap['œ'] = 'oe'; foldMap['Œ'] = 'OE';
function baseLetters(str) {
  let out = '';
  for (const ch of str) out += hop.call(foldMap, ch) ? foldMap[ch] : ch;
  return out.replace(/[̀-ͯ]/g, '');
}
function collKey(c, s) {
  let t = s;
  if (c.ignorePunctuation) t = t.replace(/[\s!-\/:-@\[-`{-~]/g, '');
  return t;
}
const PUNCT = '_-,;:!?.\'"()[]{}@*/\\&#%`^+<=>|~$';
function cmpPrimary(a, b, numeric) {
  a = baseLetters(a).toLowerCase();
  b = baseLetters(b).toLowerCase();
  let i = 0, j = 0;
  const kind = (ch) => {
    if (ch === undefined) return 0;
    if (/\s/.test(ch)) return 1;
    if (/[0-9]/.test(ch)) return 3;
    if (/[A-Za-zÀ-￿]/.test(ch)) return 4;
    return 2; // punctuation and symbols sort before digits and letters
  };
  while (i < a.length && j < b.length) {
    const ca = a[i], cb = b[j];
    if (numeric && kind(ca) === 3 && kind(cb) === 3) {
      let ei = i, ej = j;
      while (ei < a.length && /[0-9]/.test(a[ei])) ei++;
      while (ej < b.length && /[0-9]/.test(b[ej])) ej++;
      const na = a.slice(i, ei).replace(/^0+(?=\d)/, ''), nb = b.slice(j, ej).replace(/^0+(?=\d)/, '');
      if (na.length !== nb.length) return na.length < nb.length ? -1 : 1;
      if (na !== nb) return na < nb ? -1 : 1;
      i = ei; j = ej;
      continue;
    }
    if (ca !== cb) {
      const ka = kind(ca), kb = kind(cb);
      if (ka !== kb) return ka < kb ? -1 : 1;
      if (ka === 2) {
        const ra = PUNCT.indexOf(ca), rb = PUNCT.indexOf(cb);
        const xa = ra < 0 ? 100 + ca.charCodeAt(0) : ra, xb = rb < 0 ? 100 + cb.charCodeAt(0) : rb;
        return xa < xb ? -1 : 1;
      }
      return ca < cb ? -1 : 1;
    }
    i++; j++;
  }
  if (i < a.length) return 1;
  if (j < b.length) return -1;
  return 0;
}
function accentsOf(s) {
  // the accents (marks) after each base letter, for the secondary comparison
  return s.normalize ? s : s;
}
function cmpAccent(a, b) {
  const sa = a.replace(/[A-Z]/g, (c) => c.toLowerCase()), sb = b.replace(/[A-Z]/g, (c) => c.toLowerCase());
  if (sa === sb) return 0;
  const la = baseLetters(sa), lb = baseLetters(sb);
  // the same base letters: those with the accent come after
  for (let i = 0; i < Math.min(sa.length, sb.length); i++) {
    const x = sa[i], y = sb[i];
    if (x !== y) {
      const ax = hop.call(foldMap, x) || /[̀-ͯ]/.test(x), ay = hop.call(foldMap, y) || /[̀-ͯ]/.test(y);
      if (ax !== ay) return ax ? 1 : -1;
      return x < y ? -1 : 1;
    }
  }
  return sa.length < sb.length ? -1 : sa.length > sb.length ? 1 : 0;
}
function cmpCase(a, b, upperFirst) {
  for (let i = 0; i < Math.min(a.length, b.length); i++) {
    if (a[i] !== b[i]) {
      const ua = a[i] !== a[i].toLowerCase(), ub = b[i] !== b[i].toLowerCase();
      if (ua !== ub) return (ua ? 1 : -1) * (upperFirst ? -1 : 1);
    }
  }
  return 0;
}
class Collator {
  constructor(locales, options) {
    const loc = resolveLocale(locales);
    const o = getOpts(options);
    const s = { brand: 'Collator', locale: loc };
    s.usage = optStr(o, 'usage', ['sort', 'search'], 'sort');
    s.sensitivity = optStr(o, 'sensitivity', ['base', 'accent', 'case', 'variant'], 'variant');
    s.ignorePunctuation = optBool(o, 'ignorePunctuation', false);
    s.numeric = optBool(o, 'numeric', false);
    s.caseFirst = optStr(o, 'caseFirst', ['upper', 'lower', 'false'], 'false');
    s.collation = 'default';
    setSlots(this, s);
  }
  get compare() {
    const s = slots(this, 'Collator');
    if (!s.bound) {
      s.bound = (a, b) => {
        a = collKey(s, String(a));
        b = collKey(s, String(b));
        let r = cmpPrimary(a, b, s.numeric);
        if (r !== 0) return r;
        if (s.sensitivity === 'accent' || s.sensitivity === 'variant') {
          r = cmpAccent(a, b);
          if (r !== 0) return r;
        }
        if (s.sensitivity === 'case' || s.sensitivity === 'variant') {
          r = cmpCase(a, b, s.caseFirst === 'upper');
          if (r !== 0) return r;
        }
        return 0;
      };
    }
    return s.bound;
  }
  resolvedOptions() {
    const s = slots(this, 'Collator');
    return { locale: s.locale, usage: s.usage, sensitivity: s.sensitivity, ignorePunctuation: s.ignorePunctuation, collation: 'default', numeric: s.numeric, caseFirst: s.caseFirst };
  }
  static supportedLocalesOf(l) { return canonLocales(l); }
}
tag(Collator, 'Intl.Collator');
enumerable(Collator, ['compare', 'resolvedOptions']);

// ---- PluralRules ----

class PluralRules {
  constructor(locales, options) {
    const loc = resolveLocale(locales);
    const o = getOpts(options);
    setSlots(this, { brand: 'PluralRules', locale: loc, type: optStr(o, 'type', ['cardinal', 'ordinal'], 'cardinal'), nf: new NumberFormat('en', { useGrouping: false, minimumFractionDigits: o.minimumFractionDigits, maximumFractionDigits: o.maximumFractionDigits, minimumSignificantDigits: o.minimumSignificantDigits, maximumSignificantDigits: o.maximumSignificantDigits }) });
  }
  select(n) {
    const s = slots(this, 'PluralRules');
    n = Number(n);
    if (n !== n || n === Infinity || n === -Infinity) return 'other';
    const text = s.nf.format(Math.abs(n));
    const hasFrac = text.indexOf('.') >= 0;
    const int = Math.abs(Math.trunc(Number(text)));
    if (s.type === 'ordinal') {
      if (hasFrac) return 'other';
      const m10 = int % 10, m100 = int % 100;
      if (m10 === 1 && m100 !== 11) return 'one';
      if (m10 === 2 && m100 !== 12) return 'two';
      if (m10 === 3 && m100 !== 13) return 'few';
      return 'other';
    }
    return int === 1 && !hasFrac ? 'one' : 'other';
  }
  selectRange(a, b) {
    return this.select(b);
  }
  resolvedOptions() {
    const s = slots(this, 'PluralRules');
    const o = s.nf.resolvedOptions();
    return { locale: s.locale, type: s.type, minimumIntegerDigits: 1, minimumFractionDigits: o.minimumFractionDigits, maximumFractionDigits: o.maximumFractionDigits, pluralCategories: s.type === 'ordinal' ? ['few', 'one', 'two', 'other'] : ['one', 'other'] };
  }
  static supportedLocalesOf(l) { return canonLocales(l); }
}
tag(PluralRules, 'Intl.PluralRules');
enumerable(PluralRules, ['select', 'selectRange', 'resolvedOptions']);

// ---- RelativeTimeFormat ----

const RT_UNITS = ['second', 'minute', 'hour', 'day', 'week', 'month', 'quarter', 'year'];
class RelativeTimeFormat {
  constructor(locales, options) {
    const loc = resolveLocale(locales);
    const o = getOpts(options);
    setSlots(this, { brand: 'RelativeTimeFormat', locale: loc, style: optStr(o, 'style', ['long', 'short', 'narrow'], 'long'), numeric: optStr(o, 'numeric', ['always', 'auto'], 'always'), nf: new NumberFormat('en') });
  }
  format(value, unit) {
    return this.formatToParts(value, unit).map((p) => p.value).join('');
  }
  formatToParts(value, unit) {
    const s = slots(this, 'RelativeTimeFormat');
    value = Number(value);
    if (!isFinite(value)) throw new RangeError('Value need to be finite number for Intl.RelativeTimeFormat.prototype.format()');
    unit = String(unit).replace(/s$/, '');
    if (RT_UNITS.indexOf(unit) < 0) throw new RangeError("Invalid unit argument for format() '" + unit + "'");
    const neg = value < 0 || (value === 0 && 1 / value < 0);
    const abs = Math.abs(value);
    if (s.numeric === 'auto') {
      const special = {
        day: { '-1': 'yesterday', '0': 'today', '1': 'tomorrow' },
        year: { '-1': 'last year', '0': 'this year', '1': 'next year' },
        month: { '-1': 'last month', '0': 'this month', '1': 'next month' },
        week: { '-1': 'last week', '0': 'this week', '1': 'next week' },
        quarter: { '-1': 'last quarter', '0': 'this quarter', '1': 'next quarter' },
        hour: { '0': 'this hour' }, minute: { '0': 'this minute' }, second: { '0': 'now' },
      }[unit];
      const key = String(neg && abs === 0 ? 0 : value);
      if (special && hop.call(special, key)) return [{ type: 'literal', value: special[key] }];
    }
    const names = s.style === 'long' ? { second: 'second', minute: 'minute', hour: 'hour', day: 'day', week: 'week', month: 'month', quarter: 'quarter', year: 'year' } : { second: 'sec.', minute: 'min.', hour: 'hr.', day: 'day', week: 'wk.', month: 'mo.', quarter: 'qtr.', year: 'yr.' };
    let name = names[unit];
    const one = abs === 1;
    if (!one) name = s.style === 'long' ? name + 's' : unit === 'day' ? 'days' : unit === 'quarter' ? 'qtrs.' : name;
    const num = s.nf.formatToParts(abs).map((p) => Object.assign({ unit }, p));
    const out = [];
    if (!neg) out.push({ type: 'literal', value: 'in ' });
    for (const p of num) out.push(p);
    out.push({ type: 'literal', value: ' ' + name + (neg ? ' ago' : '') });
    return out;
  }
  resolvedOptions() {
    const s = slots(this, 'RelativeTimeFormat');
    return { locale: s.locale, style: s.style, numeric: s.numeric, numberingSystem: 'latn' };
  }
  static supportedLocalesOf(l) { return canonLocales(l); }
}
tag(RelativeTimeFormat, 'Intl.RelativeTimeFormat');
enumerable(RelativeTimeFormat, ['format', 'formatToParts', 'resolvedOptions']);

// ---- ListFormat ----

class ListFormat {
  constructor(locales, options) {
    const loc = resolveLocale(locales);
    const o = getOpts(options);
    setSlots(this, { brand: 'ListFormat', locale: loc, type: optStr(o, 'type', ['conjunction', 'disjunction', 'unit'], 'conjunction'), style: optStr(o, 'style', ['long', 'short', 'narrow'], 'long') });
  }
  format(list) {
    return this.formatToParts(list).map((p) => p.value).join('');
  }
  formatToParts(list) {
    const s = slots(this, 'ListFormat');
    const items = [];
    if (list !== undefined) {
      for (const x of list) {
        if (typeof x !== 'string') throw new TypeError('Iterable yielded ' + String(x) + ' which is not a string');
        items.push(x);
      }
    }
    const out = [];
    const word = s.type === 'disjunction' ? 'or' : s.type === 'unit' ? '' : s.style === 'long' ? 'and' : s.style === 'short' ? '&' : '';
    const sep = s.type === 'unit' ? (s.style === 'narrow' ? ' ' : ', ') : ', ';
    items.forEach((it, i) => {
      if (i > 0) {
        let l;
        if (i === items.length - 1) {
          if (s.type === 'unit') l = s.style === 'narrow' ? ' ' : ', ';
          else if (!word) l = ', ';
          else l = items.length === 2 ? ' ' + word + ' ' : ', ' + word + ' ';
        } else l = sep;
        out.push({ type: 'literal', value: l });
      }
      out.push({ type: 'element', value: it });
    });
    return out;
  }
  resolvedOptions() {
    const s = slots(this, 'ListFormat');
    return { locale: s.locale, type: s.type, style: s.style };
  }
  static supportedLocalesOf(l) { return canonLocales(l); }
}
tag(ListFormat, 'Intl.ListFormat');
enumerable(ListFormat, ['format', 'formatToParts', 'resolvedOptions']);

// ---- Locale ----

class Locale {
  constructor(tag, options) {
    if (arguments.length === 0 || (typeof tag !== 'string' && !isObj(tag))) throw new TypeError('First argument to Intl.Locale constructor can\'t be empty or missing');
    const c = canonLocale(hasBrand(tag, 'Locale') ? tag.toString() : tag);
    const p = c.split('-');
    const s = { brand: 'Locale', language: p[0], script: undefined, region: undefined };
    for (let i = 1; i < p.length; i++) {
      if (p[i].length === 4 && /^[A-Za-z]+$/.test(p[i])) s.script = p[i];
      else if (p[i].length === 2 || /^\d{3}$/.test(p[i])) { s.region = p[i]; break; }
    }
    const o = getOpts(options);
    if (o.language !== undefined) s.language = String(o.language).toLowerCase();
    if (o.script !== undefined) s.script = String(o.script);
    if (o.region !== undefined) s.region = String(o.region).toUpperCase();
    setSlots(this, s);
  }
  get language() { return slots(this, 'Locale').language; }
  get script() { return slots(this, 'Locale').script; }
  get region() { return slots(this, 'Locale').region; }
  get baseName() { const s = slots(this, 'Locale'); return [s.language, s.script, s.region].filter(Boolean).join('-'); }
  toString() { return this.baseName; }
  maximize() { const s = slots(this, 'Locale'); return new Locale([s.language, s.script || (s.language === 'en' ? 'Latn' : undefined), s.region || (s.language === 'en' ? 'US' : undefined)].filter(Boolean).join('-')); }
  minimize() { const s = slots(this, 'Locale'); return new Locale(s.language); }
}
tag(Locale, 'Intl.Locale');
enumerable(Locale, ['language', 'script', 'region', 'baseName', 'toString', 'maximize', 'minimize']);

// ---- DisplayNames ----

const REGION_NAMES = { US: 'United States', GB: 'United Kingdom', CA: 'Canada', AU: 'Australia', DE: 'Germany', FR: 'France', ES: 'Spain', IT: 'Italy', JP: 'Japan', CN: 'China', IN: 'India', BR: 'Brazil', MX: 'Mexico', RU: 'Russia', KR: 'South Korea', NL: 'Netherlands', SE: 'Sweden', NO: 'Norway', DK: 'Denmark', FI: 'Finland', PL: 'Poland', CH: 'Switzerland', AT: 'Austria', BE: 'Belgium', IE: 'Ireland', NZ: 'New Zealand', ZA: 'South Africa', AR: 'Argentina', TR: 'Türkiye', IL: 'Israel', SG: 'Singapore', HK: 'Hong Kong SAR China', TW: 'Taiwan', UA: 'Ukraine', PT: 'Portugal', GR: 'Greece', EG: 'Egypt', SA: 'Saudi Arabia', AE: 'United Arab Emirates' };
const LANGUAGE_NAMES = { en: 'English', de: 'German', fr: 'French', es: 'Spanish', it: 'Italian', pt: 'Portuguese', ja: 'Japanese', zh: 'Chinese', ko: 'Korean', ru: 'Russian', ar: 'Arabic', hi: 'Hindi', nl: 'Dutch', sv: 'Swedish', no: 'Norwegian', da: 'Danish', fi: 'Finnish', pl: 'Polish', tr: 'Turkish', he: 'Hebrew', el: 'Greek', cs: 'Czech', uk: 'Ukrainian', 'en-US': 'American English', 'en-GB': 'British English', 'pt-BR': 'Brazilian Portuguese', 'es-MX': 'Mexican Spanish', 'zh-Hans': 'Simplified Chinese', 'zh-Hant': 'Traditional Chinese' };
const SCRIPT_NAMES = { Latn: 'Latin', Cyrl: 'Cyrillic', Arab: 'Arabic', Hans: 'Simplified Han', Hant: 'Traditional Han', Grek: 'Greek', Hebr: 'Hebrew', Deva: 'Devanagari', Jpan: 'Japanese', Kore: 'Korean' };
class DisplayNames {
  constructor(locales, options) {
    const loc = resolveLocale(locales);
    const o = getOpts(options);
    if (o.type === undefined) throw new TypeError('Required option "type" is missing');
    setSlots(this, { brand: 'DisplayNames', locale: loc, type: optStr(o, 'type', ['language', 'region', 'script', 'currency', 'calendar', 'dateTimeField'], undefined), style: optStr(o, 'style', ['narrow', 'short', 'long'], 'long'), fallback: optStr(o, 'fallback', ['code', 'none'], 'code') });
  }
  of(code) {
    const s = slots(this, 'DisplayNames');
    code = String(code);
    let name;
    if (s.type === 'region') name = REGION_NAMES[code.toUpperCase()];
    else if (s.type === 'language') name = LANGUAGE_NAMES[code] || LANGUAGE_NAMES[code.toLowerCase()];
    else if (s.type === 'script') name = SCRIPT_NAMES[code];
    else if (s.type === 'currency') name = hop.call(CURRENCIES, code.toUpperCase()) ? CURRENCIES[code.toUpperCase()][2].replace(/s$/, '').replace(/^US dollar$/, 'US Dollar') : undefined;
    if (name === undefined) return s.fallback === 'none' ? undefined : code;
    return name;
  }
  resolvedOptions() {
    const s = slots(this, 'DisplayNames');
    return { locale: s.locale, style: s.style, type: s.type, fallback: s.fallback };
  }
  static supportedLocalesOf(l) { return canonLocales(l); }
}
tag(DisplayNames, 'Intl.DisplayNames');
enumerable(DisplayNames, ['of', 'resolvedOptions']);

// ---- Segmenter ----

function isCombining(c) {
  return (c >= 0x300 && c <= 0x36f) || (c >= 0x1ab0 && c <= 0x1aff) || (c >= 0x1dc0 && c <= 0x1dff) || (c >= 0x20d0 && c <= 0x20ff) || (c >= 0xfe00 && c <= 0xfe0f) || (c >= 0xfe20 && c <= 0xfe2f) || c === 0x200d || (c >= 0x1f3fb && c <= 0x1f3ff) || (c >= 0xe0020 && c <= 0xe007f);
}
function isWordChar(c) {
  return (c >= 48 && c <= 57) || (c >= 65 && c <= 90) || (c >= 97 && c <= 122) || c === 95 || (c >= 0xc0 && c !== 0xd7 && c !== 0xf7 && c < 0x2000) || (c >= 0x3040 && c < 0xe000) || c > 0xffff;
}
function segments(str, gran) {
  const out = [];
  const cps = Array.from(str);
  let idx = 0;
  const push = (text, extra) => {
    out.push(Object.assign({ segment: text, index: idx, input: str }, extra));
    idx += text.length;
  };
  if (gran === 'grapheme') {
    let i = 0;
    while (i < cps.length) {
      let text = cps[i++];
      const first = text.codePointAt(0);
      // regional indicator pairs
      if (first >= 0x1f1e6 && first <= 0x1f1ff && i < cps.length) {
        const n = cps[i].codePointAt(0);
        if (n >= 0x1f1e6 && n <= 0x1f1ff) text += cps[i++];
      }
      if (first === 13 && i < cps.length && cps[i] === '\n') text += cps[i++];
      while (i < cps.length) {
        const c = cps[i].codePointAt(0);
        if (isCombining(c)) {
          text += cps[i++];
          if (c === 0x200d && i < cps.length) text += cps[i++];
        } else break;
      }
      push(text);
    }
  } else if (gran === 'word') {
    let i = 0;
    while (i < cps.length) {
      const c = cps[i].codePointAt(0);
      let text = cps[i++];
      if (isWordChar(c)) {
        while (i < cps.length) {
          const d = cps[i].codePointAt(0);
          if (isWordChar(d) || isCombining(d) || ((d === 39 || d === 46 || d === 0x2019) && i + 1 < cps.length && isWordChar(cps[i + 1].codePointAt(0)) && isWordChar(cps[i - 1].codePointAt(0)) && !(d === 46 && !/[0-9]/.test(cps[i - 1])))) text += cps[i++];
          else break;
        }
        push(text, { isWordLike: true });
      } else if (/\s/.test(text)) {
        while (i < cps.length && /\s/.test(cps[i])) text += cps[i++];
        push(text, { isWordLike: false });
      } else push(text, { isWordLike: false });
    }
  } else {
    let start = 0;
    const s = str;
    const re = /[^.!?]*(?:[.!?]+["')\]]*(?:\s+|$)|$)/g;
    let m;
    while ((m = re.exec(s)) && m[0] !== '') {
      push(m[0]);
      start += m[0].length;
    }
  }
  return out;
}
class Segmenter {
  constructor(locales, options) {
    const loc = resolveLocale(locales);
    const o = getOpts(options);
    setSlots(this, { brand: 'Segmenter', locale: loc, granularity: optStr(o, 'granularity', ['grapheme', 'word', 'sentence'], 'grapheme') });
  }
  segment(input) {
    const s = slots(this, 'Segmenter');
    const str = String(input);
    const list = segments(str, s.granularity);
    const res = {
      containing(i) {
        i = Math.trunc(Number(i)) || 0;
        if (i < 0 || i >= str.length) return undefined;
        return list.find((x) => i >= x.index && i < x.index + x.segment.length);
      },
      [SymIter]() {
        return list[SymIter]();
      },
    };
    return res;
  }
  resolvedOptions() {
    const s = slots(this, 'Segmenter');
    return { locale: s.locale, granularity: s.granularity };
  }
  static supportedLocalesOf(l) { return canonLocales(l); }
}
tag(Segmenter, 'Intl.Segmenter');
enumerable(Segmenter, ['segment', 'resolvedOptions']);

// ---- the Intl object ----

const Intl = {};
for (const [k, v] of Object.entries({ NumberFormat, DateTimeFormat, Collator, PluralRules, RelativeTimeFormat, ListFormat, Locale, DisplayNames, Segmenter })) {
  ObjDefine(Intl, k, { value: v, writable: true, configurable: true });
}
ObjDefine(Intl, 'getCanonicalLocales', { value: (l) => canonLocales(l), writable: true, configurable: true });
ObjDefine(Intl, 'supportedValuesOf', {
  value(key) {
    key = String(key);
    if (key === 'calendar') return ['gregory'];
    if (key === 'collation') return ['default'];
    if (key === 'currency') return Object.keys(CURRENCIES).sort();
    if (key === 'numberingSystem') return ['latn'];
    if (key === 'timeZone') return Object.keys(ZONES).sort();
    if (key === 'unit') return Object.keys(UNITS).sort();
    throw new RangeError('Invalid key : ' + key);
  },
  writable: true, configurable: true,
});
ObjDefine(Intl, TAG, { value: 'Intl', configurable: true });

// Number.prototype.toLocaleString and the Date ones, with locales and options.
function numberToLocale(x, locales, options) {
  return new NumberFormat(locales, options).format(x);
}
function dateToLocale(d, locales, options, defaults, required) {
  const t = d.getTime();
  if (t !== t) return 'Invalid Date';
  const o = Object.assign(Object.create(null), isObj(options) ? options : {});
  const has = (ks) => ks.some((k) => o[k] !== undefined);
  const dateKeys = ['weekday', 'year', 'month', 'day'], timeKeys = ['dayPeriod', 'hour', 'minute', 'second', 'fractionalSecondDigits'];
  let need = true;
  if (required === 'date' || required === 'any') if (has(dateKeys)) need = false;
  if (required === 'time' || required === 'any') if (has(timeKeys)) need = false;
  if (o.dateStyle !== undefined || o.timeStyle !== undefined) need = false;
  if (required === 'date' && o.timeStyle !== undefined) throw new TypeError('Invalid option : timeStyle');
  if (required === 'time' && o.dateStyle !== undefined) throw new TypeError('Invalid option : dateStyle');
  o.__defaults = need ? defaults : 'none';
  return new DateTimeFormat(locales, o).format(t);
}
function localeCompare(a, b, locales, options) {
  return new Collator(locales, options).compare(a, b);
}
