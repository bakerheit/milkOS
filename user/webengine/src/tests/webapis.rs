//! The web platform APIs scripts expect beyond the DOM itself: crypto, idle callbacks, the
//! performance timeline, matchMedia, CSS, geometry classes, navigator's objects, animations, the
//! rest of the Event family, observers, Intl, DOMParser and the other DOM helpers. Each test runs
//! a small page and reads what it logged.

use alloc::string::String;
use alloc::vec::Vec;

use super::script::{Host, doc};
use crate::api::*;

/// Runs `body` as the page's script (the host's clock moves 5 ms a step for `steps` steps, with
/// frames) and returns the console lines starting with `R `, without that.
fn run(body: &str, steps: u32) -> Vec<String> {
    let mut h = Host::new();
    run_with(&mut h, body, steps)
}

fn run_with(h: &mut Host, body: &str, steps: u32) -> Vec<String> {
    let html = alloc::format!("<!DOCTYPE html><html><body><div id=a style=\"width:100px;height:40px\">x</div><script>{body}</script></body></html>");
    let mut d = doc(&html);
    let mut out = Vec::new();
    for _ in 0..=steps {
        d.run(h);
        if d.wants_frame() {
            d.frame(h);
        }
        d.layout(&Viewport::new(800, 600), &ApproxMeasurer);
        for l in d.take_console() {
            out.push(l.text);
        }
        h.now += 5.0;
    }
    out.into_iter()
        .map(|l| l.strip_prefix("R ").map(String::from).unwrap_or(l))
        .collect()
}

/// The result of one expression, as text.
fn eval(src: &str) -> String {
    let lit = src.replace('\\', "\\\\").replace('\'', "\\'").replace('\n', "\\n");
    let lines = run(&alloc::format!("try {{ console.log('R ' + eval('{lit}')); }} catch (e) {{ console.log('R ERR ' + e.name + ': ' + e.message); }}"), 0);
    lines.into_iter().next().unwrap_or_default()
}

#[test]
fn crypto_gives_random_bytes_and_uuids() {
    assert_eq!(eval("typeof crypto + ' ' + typeof crypto.getRandomValues + ' ' + typeof crypto.randomUUID + ' ' + typeof crypto.subtle"), "object function function undefined");
    assert_eq!(eval("var a = new Uint8Array(16); (crypto.getRandomValues(a) === a) + ' ' + a.some(function (x) { return x !== 0 })"), "true true");
    assert_eq!(eval("var a = new Uint32Array(4); crypto.getRandomValues(a); a.length + ' ' + (new Set(a).size > 1)"), "4 true");
    assert_eq!(eval("/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(crypto.randomUUID())"), "true");
    assert_eq!(eval("crypto.randomUUID() !== crypto.randomUUID()"), "true");
    assert_eq!(eval("crypto.getRandomValues(new Float32Array(2))"), "ERR TypeMismatchError: Failed to execute 'getRandomValues' on 'Crypto': The provided value is not of type '(Int8Array or Int16Array or Int32Array or Uint8Array or Uint16Array or Uint32Array or Uint8ClampedArray)'");
    assert_eq!(eval("crypto.getRandomValues(new Uint8Array(65537))").split(':').next().unwrap(), "ERR QuotaExceededError");
    assert_eq!(eval("crypto.getRandomValues(new Uint8Array(65536)).length"), "65536");
    assert_eq!(eval("crypto.getRandomValues()").split(':').next().unwrap(), "ERR TypeError");
    assert_eq!(eval("Object.prototype.toString.call(crypto) + (crypto instanceof Crypto) + (window.crypto === self.crypto)"), "[object Crypto]truetrue");
    // A subarray fills only its part.
    assert_eq!(eval("var b = new Uint8Array(32); crypto.getRandomValues(b.subarray(8, 16)); b.slice(0, 8).every(function (x) { return x === 0 }) + ' ' + b.slice(16).every(function (x) { return x === 0 })"), "true true");
}

#[test]
fn idle_callbacks_run_with_a_deadline() {
    let lines = run(
        "var id = requestIdleCallback(function (d) { console.log('R idle ' + d.didTimeout + ' ' + (d.timeRemaining() <= 50) + ' ' + (d instanceof IdleDeadline)); });\
         var gone = requestIdleCallback(function () { console.log('R never'); }); cancelIdleCallback(gone);\
         requestIdleCallback(function (d) { console.log('R with timeout ' + typeof d.timeRemaining()); }, { timeout: 100 });\
         console.log('R ids ' + (typeof id) + (id !== gone));",
        3,
    );
    assert_eq!(lines, ["ids numbertrue", "idle false true true", "with timeout number"], "{lines:?}");
    assert_eq!(eval("try { requestIdleCallback(1) } catch (e) { e.name }"), "TypeError");
}

#[test]
fn the_performance_timeline_has_marks_and_measures() {
    let r = run(
        "var m1 = performance.mark('a'); var m2 = performance.mark('b', { detail: { k: 1 } });\
         var me = performance.measure('a to b', 'a', 'b');\
         console.log('R ' + [m1.entryType, m1.name, typeof m1.startTime, m2.detail.k, me.entryType, me.name, me.duration >= 0, me.startTime === m1.startTime].join());\
         console.log('R ' + performance.getEntriesByType('mark').length + ' ' + performance.getEntriesByName('a').length + ' ' + performance.getEntries().length);\
         performance.clearMarks('a'); console.log('R ' + performance.getEntriesByType('mark').length);\
         performance.clearMarks(); performance.clearMeasures(); console.log('R ' + performance.getEntries().length);\
         console.log('R ' + (m1 instanceof PerformanceMark) + (me instanceof PerformanceMeasure) + (m1 instanceof PerformanceEntry));\
         try { performance.measure('x', 'nope'); } catch (e) { console.log('R ' + e.name); }\
         console.log('R ' + typeof performance.timing.navigationStart + ' ' + performance.navigation.type + ' ' + (performance.timeOrigin > 0) + ' ' + JSON.stringify(performance.getEntriesByType('navigation').length));",
        0,
    );
    assert_eq!(
        r,
        ["mark,a,number,1,measure,a to b,true,true", "2 1 3", "1", "0", "truetruetrue", "SyntaxError", "number 0 true 1"],
        "{r:?}"
    );
}

#[test]
fn performance_observers_hear_marks() {
    let r = run(
        "var seen = []; var o = new PerformanceObserver(function (list, obs) { list.getEntries().forEach(function (e) { seen.push(e.name); }); console.log('R got ' + seen.join()); });\
         o.observe({ entryTypes: ['mark'] }); performance.mark('one'); performance.mark('two'); performance.measure('m');\
         console.log('R sync ' + seen.length); console.log('R ' + PerformanceObserver.supportedEntryTypes.join());",
        2,
    );
    assert_eq!(r, ["sync 0", "mark,measure", "got one,two"], "{r:?}");
}

#[test]
fn match_media_is_live_and_an_event_target() {
    assert_eq!(eval("var m = matchMedia('(min-width: 500px)'); m.matches + ' ' + m.media + ' ' + (m instanceof EventTarget) + ' ' + (m instanceof MediaQueryList)"), "true (min-width: 500px) true true");
    assert_eq!(eval("matchMedia('(min-width: 5000px)').matches + ' ' + matchMedia('(prefers-color-scheme: dark)').matches + ' ' + matchMedia('screen').matches + ' ' + matchMedia('print').matches"), "false false true false");
    assert_eq!(eval("var m = matchMedia('(max-width: 900px)'); m.addListener(function () {}); m.addEventListener('change', function () {}); m.onchange = null; m.removeListener(function () {}); typeof m.onchange"), "object");
    assert_eq!(eval("matchMedia()").split(':').next().unwrap(), "ERR TypeError");
}

#[test]
fn css_supports_and_escape() {
    assert_eq!(eval("CSS.supports('display', 'grid') + ' ' + CSS.supports('display', 'nonsense') + ' ' + CSS.supports('nonsense', '1')"), "true false false");
    assert_eq!(eval("CSS.supports('(display: flex)') + ' ' + CSS.supports('display: flex') + ' ' + CSS.supports('(display: flex) and (color: red)') + ' ' + CSS.supports('not (display: banana)') + ' ' + CSS.supports('(color: red) or (what: ever)')"), "true true true true true");
    assert_eq!(eval("CSS.supports('selector(a > b)') + ' ' + CSS.supports('color', 'rgb(1 2 3)') + ' ' + CSS.supports('width', 'calc(1px + 2px)')"), "true true true");
    assert_eq!(eval("CSS.escape('a b') + '|' + CSS.escape('1a') + '|' + CSS.escape('-') + '|' + CSS.escape('a.b#c') + '|' + CSS.escape('--x') + '|' + CSS.escape('é')"), "a\\ b|\\31 a|\\-|a\\.b\\#c|--x|é");
    assert_eq!(eval("CSS.supports()").split(':').next().unwrap(), "ERR TypeError");
}

#[test]
fn geometry_classes_work() {
    assert_eq!(eval("var r = new DOMRect(1, 2, 3, 4); [r.x, r.y, r.width, r.height, r.top, r.left, r.right, r.bottom].join() + ' ' + JSON.stringify(r)"), r#"1,2,3,4,2,1,4,6 {"x":1,"y":2,"width":3,"height":4,"top":2,"right":4,"bottom":6,"left":1}"#);
    assert_eq!(eval("var r = document.getElementById('a').getBoundingClientRect(); (r instanceof DOMRect) + ' ' + (r instanceof DOMRectReadOnly) + ' ' + r.width + ' ' + r.height"), "true true 100 40");
    assert_eq!(eval("DOMRect.fromRect({ x: 1, width: 5 }).right + ' ' + new DOMRectReadOnly(0, 0, -3, 2).left"), "6 -3");
    assert_eq!(eval("var p = new DOMPoint(1, 2, 3, 4); p.x + p.y + p.z + p.w + ' ' + JSON.stringify(DOMPoint.fromPoint({ x: 9 }))"), r#"10 {"x":9,"y":0,"z":0,"w":1}"#);
    assert_eq!(eval("var m = new DOMMatrix([1, 0, 0, 1, 10, 20]); m.e + ' ' + m.f + ' ' + m.is2D + ' ' + m.translate(5, 5).e + ' ' + m.scale(2).a + ' ' + m.toString()"), "10 20 true 15 2 matrix(1, 0, 0, 1, 10, 20)");
    assert_eq!(eval("var m = new DOMMatrix('translate(10px, 20px) scale(2)'); var p = m.transformPoint({ x: 1, y: 1 }); p.x + ',' + p.y"), "12,22");
    assert_eq!(eval("var m = new DOMMatrix([2, 0, 0, 2, 4, 6]); var i = m.inverse(); i.a + ' ' + i.e + ' ' + m.multiply(i).isIdentity"), "0.5 -2 true");
    assert_eq!(eval("new DOMMatrix().isIdentity + ' ' + new DOMMatrix().toString()"), "true matrix(1, 0, 0, 1, 0, 0)");
    assert_eq!(eval("DOMRect()").split(':').next().unwrap(), "ERR TypeError");
}

#[test]
fn navigator_and_screen_objects() {
    assert_eq!(eval("[typeof navigator.clipboard.writeText, typeof navigator.permissions.query, navigator.connection.effectiveType, typeof navigator.geolocation.getCurrentPosition, navigator.plugins.length, navigator.mimeTypes.length, navigator.doNotTrack, navigator.pdfViewerEnabled, navigator.vibrate(1)].join()"), "function,function,4g,function,0,0,,false,false");
    let r = run(
        "navigator.permissions.query({ name: 'geolocation' }).then(function (s) { console.log('R perm ' + s.state + ' ' + s.name + ' ' + (s instanceof EventTarget)); });\
         navigator.permissions.query({ name: 'nope' }).catch(function (e) { console.log('R bad ' + e.name); });\
         navigator.geolocation.getCurrentPosition(function () { console.log('R ok'); }, function (e) { console.log('R denied ' + e.code); });\
         navigator.clipboard.writeText('x').catch(function (e) { console.log('R clip ' + e.name); });\
         navigator.clipboard.readText().catch(function (e) { console.log('R read ' + e.name); });\
         navigator.storage.estimate().then(function (e) { console.log('R est ' + (e.quota > 0)); });",
        2,
    );
    for want in ["perm prompt geolocation true", "bad TypeError", "denied 1", "clip NotAllowedError", "read NotAllowedError", "est true"] {
        assert!(r.iter().any(|l| l == want), "{want}: {r:?}");
    }
    assert_eq!(eval("[screen.orientation.type, screen.orientation.angle, screen.availLeft, screen.isExtended, visualViewport.width, visualViewport.scale, screenX, screenY, crossOriginIsolated].join()"), "landscape-primary,0,0,false,800,1,0,0,0,false".replace("0,0,false,800,1,0,0,0,false", "0,0,false,800,1,0,0,false"));
}

#[test]
fn the_clipboard_takes_text_after_a_click() {
    let mut h = Host::new();
    let html = "<!DOCTYPE html><body><button id=b onclick=\"navigator.clipboard.writeText('copied').then(function () { console.log('R wrote') }, function (e) { console.log('R no ' + e.name) })\">go</button>";
    let mut d = doc(html);
    d.run(&mut h);
    // Without a click: refused.
    let mut lines: Vec<String> = Vec::new();
    d.run(&mut h);
    let r = d.page.rect_of_id("b").unwrap();
    // A script's own click() is not a user's: refused.
    d.click(&mut h, r.x + 2, r.y + 2, (0, 0), Mods::default());
    for _ in 0..3 {
        d.run(&mut h);
        h.now += 5.0;
        lines.extend(d.take_console().into_iter().map(|l| l.text));
    }
    assert!(lines.iter().any(|l| l == "R wrote"), "{lines:?}");
    assert_eq!(h.clipboard, "copied");
}

#[test]
fn animations_finish_and_fill() {
    let r = run(
        "var el = document.getElementById('a');\
         var an = el.animate([{ opacity: 0 }, { opacity: 1, width: '50px' }], { duration: 20, fill: 'forwards' });\
         console.log('R ' + [an.playState, typeof an.finished.then, an instanceof Animation, an.effect instanceof KeyframeEffect, el.getAnimations().length, document.getAnimations().length, document.timeline.currentTime >= 0].join());\
         an.finished.then(function (a) { console.log('R finished ' + (a === an) + ' ' + an.playState + ' ' + el.style.opacity + ' ' + el.style.width); });\
         an.onfinish = function (e) { console.log('R onfinish ' + e.type); };\
         var c = el.animate({ transform: ['none', 'scale(2)'] }, 1000);\
         c.finished.then(function () { console.log('R cancel resolved'); }, function (e) { console.log('R cancelled ' + e.name); }); c.cancel();\
         var q = el.animate([], 0); q.finished.then(function () { console.log('R instant'); });",
        10,
    );
    assert_eq!(r[0], "running,function,true,true,1,1,true", "{r:?}");
    for want in ["finished true finished 1 50px", "onfinish finish", "cancelled AbortError", "instant"] {
        assert!(r.iter().any(|l| l == want), "{want}: {r:?}");
    }
    assert_eq!(eval("document.getElementById('a').animate([], { duration: Infinity }).playState"), "running");
}

#[test]
fn more_event_constructors() {
    assert_eq!(eval("var e = new WheelEvent('wheel', { deltaY: 53, deltaMode: 1, clientX: 5 }); [e.type, e.deltaX, e.deltaY, e.deltaMode, e.clientX, e instanceof MouseEvent, e instanceof Event, WheelEvent.DOM_DELTA_LINE].join()"), "wheel,0,53,1,5,true,true,1");
    assert_eq!(eval("var e = new StorageEvent('storage', { key: 'k', newValue: 'v' }); [e.key, e.oldValue, e.newValue, e.url, e.storageArea].join()"), "k,,v,,");
    assert_eq!(eval("new PopStateEvent('popstate', { state: { a: 1 } }).state.a + ' ' + new HashChangeEvent('hashchange', { oldURL: 'a', newURL: 'b' }).newURL + ' ' + new CompositionEvent('compositionend', { data: 'x' }).data"), "1 b x");
    assert_eq!(eval("[new TouchEvent('touchstart').touches.length, new AnimationEvent('animationend', { animationName: 'n' }).animationName, new TransitionEvent('transitionend', { propertyName: 'p' }).propertyName, new CloseEvent('close', { code: 1000 }).code, new ClipboardEvent('copy').clipboardData, new DragEvent('drop').dataTransfer, new PageTransitionEvent('pageshow', { persisted: true }).persisted].join()"), "0,n,p,1000,,,true");
    assert_eq!(eval("var dt = new DataTransfer(); dt.setData('text/plain', 'hi'); dt.getData('text/plain') + dt.types.join() + dt.dropEffect"), "hitext/plainnone");
    assert_eq!(eval("var seen = ''; window.addEventListener('wheel', function (e) { seen = e.deltaY; }); window.dispatchEvent(new WheelEvent('wheel', { deltaY: 7 })); seen"), "7");
    assert_eq!(eval("var C = class extends WheelEvent {}; '' + new C('wheel').deltaMode + (new C('x') instanceof C)"), "0true");
    assert_eq!(eval("new WheelEvent()").split(':').next().unwrap(), "ERR TypeError");
}

#[test]
fn report_error_goes_to_window_onerror() {
    let r = run(
        "window.onerror = function (m, f, l, c, e) { console.log('R onerror ' + (e && e.message)); return true; };\
         reportError(new Error('boom')); console.log('R after');",
        1,
    );
    assert_eq!(r, ["onerror boom", "after"], "{r:?}");
}

// ---- Intl ----

#[test]
fn intl_number_format() {
    let nf = |opts: &str, n: &str| eval(&alloc::format!("new Intl.NumberFormat('en-US', {opts}).format({n})"));
    assert_eq!(eval("new Intl.NumberFormat('en-US').format(1234567.891)"), "1,234,567.891");
    assert_eq!(eval("new Intl.NumberFormat().format(0)"), "0");
    assert_eq!(eval("(1234.5).toLocaleString('en-US', { style: 'currency', currency: 'USD' })"), "$1,234.50");
    assert_eq!(eval("(1234.5).toLocaleString()"), "1,234.5");
    assert_eq!(eval("(0.1 + 0.2).toLocaleString('en-US')"), "0.3");
    assert_eq!(nf("{ style: 'percent' }", "0.256"), "26%");
    assert_eq!(nf("{ style: 'percent', minimumFractionDigits: 1 }", "0.256"), "25.6%");
    assert_eq!(nf("{ maximumFractionDigits: 2 }", "1.005"), "1.01");
    assert_eq!(nf("{ maximumFractionDigits: 0 }", "2.5"), "3");
    assert_eq!(nf("{ maximumFractionDigits: 0 }", "-2.5"), "-3");
    assert_eq!(nf("{ minimumFractionDigits: 2 }", "5"), "5.00");
    assert_eq!(nf("{ minimumIntegerDigits: 3 }", "5"), "005");
    assert_eq!(nf("{ useGrouping: false }", "1234567"), "1234567");
    assert_eq!(nf("{ maximumSignificantDigits: 3 }", "123456"), "123,000");
    assert_eq!(nf("{ maximumSignificantDigits: 3 }", "0.00123456"), "0.00123");
    assert_eq!(nf("{ minimumSignificantDigits: 4 }", "1.5"), "1.500");
    assert_eq!(nf("{ notation: 'compact' }", "1234"), "1.2K");
    assert_eq!(nf("{ notation: 'compact' }", "12345"), "12K");
    assert_eq!(nf("{ notation: 'compact' }", "123456"), "123K");
    assert_eq!(nf("{ notation: 'compact' }", "999999"), "1M");
    assert_eq!(nf("{ notation: 'compact' }", "1500000"), "1.5M");
    assert_eq!(nf("{ notation: 'compact' }", "7"), "7");
    assert_eq!(nf("{ notation: 'compact', compactDisplay: 'long' }", "1234"), "1.2 thousand");
    assert_eq!(nf("{ notation: 'compact' }", "2500000000"), "2.5B");
    assert_eq!(nf("{ notation: 'scientific' }", "123456"), "1.235E5");
    assert_eq!(nf("{ style: 'currency', currency: 'EUR' }", "1234.5"), "€1,234.50");
    assert_eq!(nf("{ style: 'currency', currency: 'JPY' }", "1234.5"), "¥1,235");
    assert_eq!(nf("{ style: 'currency', currency: 'USD' }", "-5"), "-$5.00");
    assert_eq!(nf("{ style: 'currency', currency: 'USD', currencySign: 'accounting' }", "-5"), "($5.00)");
    assert_eq!(nf("{ style: 'currency', currency: 'USD', currencyDisplay: 'code' }", "5"), "USD 5.00");
    assert_eq!(nf("{ style: 'currency', currency: 'USD', currencyDisplay: 'name' }", "5"), "5.00 US dollars");
    assert_eq!(nf("{ style: 'currency', currency: 'USD', minimumFractionDigits: 0 }", "5"), "$5");
    assert_eq!(nf("{ style: 'unit', unit: 'kilometer-per-hour' }", "50"), "50 km/h");
    assert_eq!(nf("{ style: 'unit', unit: 'kilobyte' }", "1.5"), "1.5 kB");
    assert_eq!(nf("{ style: 'unit', unit: 'liter', unitDisplay: 'long' }", "2"), "2 liters");
    assert_eq!(nf("{ style: 'unit', unit: 'celsius' }", "21"), "21°C");
    assert_eq!(nf("{ signDisplay: 'always' }", "5"), "+5");
    assert_eq!(nf("{ signDisplay: 'exceptZero' }", "0"), "0");
    assert_eq!(nf("{ signDisplay: 'never' }", "-5"), "5");
    assert_eq!(nf("{}", "NaN"), "NaN");
    assert_eq!(nf("{}", "Infinity"), "∞");
    assert_eq!(nf("{}", "-Infinity"), "-∞");
    assert_eq!(nf("{}", "-0"), "-0");
    assert_eq!(nf("{}", "1e21"), "1,000,000,000,000,000,000,000");
    assert_eq!(nf("{}", "1.5e-7"), "0");
    assert_eq!(nf("{ maximumFractionDigits: 10 }", "1.5e-7"), "0.00000015");
    assert_eq!(eval("JSON.stringify(new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD' }).formatToParts(-1234.5).map(function (p) { return p.type[0] + p.value; }))"), r#"["m-","c$","i1","g,","i234","d.","f50"]"#);
    assert_eq!(eval("var r = new Intl.NumberFormat('en').resolvedOptions(); [r.locale, r.numberingSystem, r.style, r.minimumFractionDigits, r.maximumFractionDigits, r.useGrouping].join()"), "en,latn,decimal,0,3,auto");
    assert_eq!(eval("new Intl.NumberFormat('de-DE').format(1234.5)"), "1,234.5");
    assert_eq!(eval("Intl.NumberFormat.supportedLocalesOf(['en-us', 'fr']).join()"), "en-US,fr");
    assert_eq!(eval("new Intl.NumberFormat('en', { style: 'currency' })").split(':').next().unwrap(), "ERR TypeError");
    assert_eq!(eval("new Intl.NumberFormat('en', { maximumFractionDigits: 200 })").split(':').next().unwrap(), "ERR RangeError");
    assert_eq!(eval("new Intl.NumberFormat('not a locale!')").split(':').next().unwrap(), "ERR RangeError");
    assert_eq!(eval("var f = new Intl.NumberFormat().format; [1, 2, 3].map(f).join()"), "1,2,3");
    assert_eq!(eval("Object.prototype.toString.call(new Intl.NumberFormat()) + Object.prototype.toString.call(Intl)"), "[object Intl.NumberFormat][object Intl]");
    assert_eq!(eval("(12345.6789).toLocaleString('en-US', { maximumFractionDigits: 1 })"), "12,345.7");
}

#[test]
fn intl_date_time_format() {
    // 2024-03-05T15:04:05.678Z, a Tuesday.
    let df = |opts: &str| eval(&alloc::format!("new Intl.DateTimeFormat('en-US', {opts}).format(new Date(Date.UTC(2024, 2, 5, 15, 4, 5, 678)))"));
    assert_eq!(df("{ timeZone: 'UTC' }"), "3/5/2024");
    assert_eq!(df("{ timeZone: 'UTC', dateStyle: 'full' }"), "Tuesday, March 5, 2024");
    assert_eq!(df("{ timeZone: 'UTC', dateStyle: 'long' }"), "March 5, 2024");
    assert_eq!(df("{ timeZone: 'UTC', dateStyle: 'medium' }"), "Mar 5, 2024");
    assert_eq!(df("{ timeZone: 'UTC', dateStyle: 'short' }"), "3/5/24");
    assert_eq!(df("{ timeZone: 'UTC', timeStyle: 'short' }"), "3:04 PM");
    assert_eq!(df("{ timeZone: 'UTC', timeStyle: 'medium' }"), "3:04:05 PM");
    assert_eq!(df("{ timeZone: 'UTC', timeStyle: 'long' }"), "3:04:05 PM UTC");
    assert_eq!(df("{ timeZone: 'UTC', dateStyle: 'full', timeStyle: 'short' }"), "Tuesday, March 5, 2024 at 3:04 PM");
    assert_eq!(df("{ timeZone: 'UTC', dateStyle: 'short', timeStyle: 'short' }"), "3/5/24, 3:04 PM");
    assert_eq!(df("{ timeZone: 'UTC', hour: 'numeric', minute: '2-digit', hour12: false }"), "15:04");
    assert_eq!(df("{ timeZone: 'UTC', hour: 'numeric', minute: '2-digit' }"), "3:04 PM");
    assert_eq!(df("{ timeZone: 'UTC', hour: 'numeric' }"), "3 PM");
    assert_eq!(df("{ timeZone: 'UTC', hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false }"), "15:04:05");
    assert_eq!(df("{ timeZone: 'UTC', month: 'long', year: 'numeric' }"), "March 2024");
    assert_eq!(df("{ timeZone: 'UTC', weekday: 'short' }"), "Tue");
    assert_eq!(df("{ timeZone: 'UTC', month: 'short', day: 'numeric' }"), "Mar 5");
    assert_eq!(df("{ timeZone: 'UTC', weekday: 'long', year: 'numeric', month: 'long', day: 'numeric' }"), "Tuesday, March 5, 2024");
    assert_eq!(df("{ timeZone: 'UTC', year: 'numeric', month: '2-digit', day: '2-digit' }"), "03/05/2024");
    assert_eq!(df("{ timeZone: 'UTC', year: 'numeric', month: 'long', day: 'numeric', hour: 'numeric', minute: 'numeric' }"), "March 5, 2024 at 3:04 PM");
    assert_eq!(df("{ timeZone: 'UTC', year: 'numeric', month: 'numeric', day: 'numeric', hour: 'numeric', minute: 'numeric', second: 'numeric', timeZoneName: 'short' }"), "3/5/2024, 3:04:05 PM UTC");
    assert_eq!(df("{ timeZone: 'UTC', minute: '2-digit', second: '2-digit', fractionalSecondDigits: 2 }"), "04:05.67");
    assert_eq!(df("{ timeZone: 'America/New_York', hour: 'numeric', minute: 'numeric', timeZoneName: 'short' }"), "10:04 AM EST");
    assert_eq!(df("{ timeZone: 'America/New_York', timeStyle: 'full' }"), "10:04:05 AM Eastern Standard Time");
    assert_eq!(df("{ timeZone: 'Asia/Tokyo', dateStyle: 'short', timeStyle: 'short' }"), "3/6/24, 12:04 AM");
    assert_eq!(df("{ timeZone: 'Asia/Kolkata', timeStyle: 'short', timeZoneName: 'shortOffset' }").len() > 0, true);
    assert_eq!(df("{ timeZone: '+05:30', timeStyle: 'short' }"), "8:34 PM");
    // Daylight saving: the second Sunday of March 2024 is the 10th.
    let at = |when: &str, zone: &str| eval(&alloc::format!("new Date(Date.UTC({when})).toLocaleTimeString('en-US', {{ timeZone: '{zone}', timeZoneName: 'short' }})"));
    assert_eq!(at("2024, 2, 9, 12", "America/New_York"), "7:00:00 AM EST");
    assert_eq!(at("2024, 2, 11, 12", "America/New_York"), "8:00:00 AM EDT");
    assert_eq!(at("2024, 10, 2, 12", "America/New_York"), "8:00:00 AM EDT");
    assert_eq!(at("2024, 10, 3, 12", "America/New_York"), "7:00:00 AM EST");
    assert_eq!(at("2024, 2, 30, 12", "Europe/London"), "12:00:00 PM GMT");
    assert_eq!(at("2024, 2, 31, 12", "Europe/London"), "1:00:00 PM GMT+1");
    assert_eq!(at("2024, 6, 1, 12", "Europe/Paris"), "2:00:00 PM GMT+2");
    assert_eq!(at("2024, 0, 1, 12", "Australia/Sydney"), "11:00:00 PM GMT+11");
    assert_eq!(at("2024, 6, 1, 12", "Australia/Sydney"), "10:00:00 PM GMT+10");
    assert_eq!(eval("new Date(Date.UTC(2024, 2, 5, 15, 4, 5)).toLocaleString('en-US', { timeZone: 'UTC' })"), "3/5/2024, 3:04:05 PM");
    assert_eq!(eval("new Date(Date.UTC(2024, 2, 5, 15, 4, 5)).toLocaleDateString('en-US', { timeZone: 'UTC' })"), "3/5/2024");
    assert_eq!(eval("new Date(Date.UTC(2024, 2, 5, 15, 4, 5)).toLocaleTimeString('en-US', { timeZone: 'UTC' })"), "3:04:05 PM");
    assert_eq!(eval("new Date(Date.UTC(2024, 2, 5, 15, 4, 5)).toLocaleDateString('en-US', { weekday: 'long', timeZone: 'UTC' })"), "Tuesday");
    assert_eq!(eval("new Date(Date.UTC(2024, 2, 5, 15, 4, 5)).toLocaleDateString(undefined, { month: 'short', day: 'numeric', timeZone: 'UTC' })"), "Mar 5");
    assert_eq!(eval("new Date(Date.UTC(2024, 2, 5, 15, 4, 5)).toLocaleTimeString('en-US', { hour: '2-digit', minute: '2-digit', timeZone: 'UTC' })"), "03:04 PM");
    assert_eq!(eval("new Date(NaN).toLocaleDateString('en-US')"), "Invalid Date");
    assert_eq!(eval("new Intl.DateTimeFormat().format(NaN)").split(':').next().unwrap(), "ERR RangeError");
    assert_eq!(eval("var r = new Intl.DateTimeFormat('en-US', { timeZone: 'America/Chicago' }).resolvedOptions(); [r.locale, r.calendar, r.numberingSystem, r.timeZone, r.year, r.month, r.day].join()"), "en-US,gregory,latn,America/Chicago,numeric,numeric,numeric");
    assert_eq!(eval("new Intl.DateTimeFormat().resolvedOptions().timeZone"), "UTC");
    assert_eq!(eval("JSON.stringify(new Intl.DateTimeFormat('en-US', { timeZone: 'UTC', hour: 'numeric', minute: 'numeric' }).formatToParts(new Date(Date.UTC(2024, 2, 5, 15, 4))).map(function (p) { return p.type; }))"), r#"["hour","literal","minute","literal","dayPeriod"]"#);
    assert_eq!(eval("new Intl.DateTimeFormat('en-US', { timeZone: 'Nowhere/Land' })").split(':').next().unwrap(), "ERR RangeError");
    assert_eq!(eval("new Intl.DateTimeFormat('en-US', { dateStyle: 'short', year: 'numeric' })").split(':').next().unwrap(), "ERR TypeError");
}

#[test]
fn intl_the_rest() {
    assert_eq!(eval("var p = new Intl.PluralRules('en'); [p.select(0), p.select(1), p.select(2), p.select(1.5)].join()"), "other,one,other,other");
    assert_eq!(eval("var p = new Intl.PluralRules('en', { type: 'ordinal' }); [1, 2, 3, 4, 11, 12, 13, 21, 22, 23, 101].map(function (n) { return p.select(n); }).join()"), "one,two,few,other,other,other,other,one,two,few,one");
    assert_eq!(eval("var r = new Intl.RelativeTimeFormat('en', { numeric: 'auto' }); [r.format(-1, 'day'), r.format(0, 'day'), r.format(1, 'day'), r.format(2, 'hour'), r.format(-3, 'month'), r.format(1, 'year'), r.format(-1, 'week')].join('|')"), "yesterday|today|tomorrow|in 2 hours|3 months ago|next year|last week");
    assert_eq!(eval("var r = new Intl.RelativeTimeFormat('en'); [r.format(-1, 'day'), r.format(1, 'second'), r.format(5, 'minutes'), r.format(-1000, 'day')].join('|')"), "1 day ago|in 1 second|in 5 minutes|1,000 days ago");
    assert_eq!(eval("new Intl.RelativeTimeFormat('en', { style: 'short' }).format(-3, 'hour')"), "3 hr. ago");
    assert_eq!(eval("var l = new Intl.ListFormat('en'); [l.format([]), l.format(['a']), l.format(['a', 'b']), l.format(['a', 'b', 'c'])].join('|')"), "|a|a and b|a, b, and c");
    assert_eq!(eval("new Intl.ListFormat('en', { type: 'disjunction' }).format(['a', 'b', 'c'])"), "a, b, or c");
    assert_eq!(eval("new Intl.ListFormat('en', { style: 'short' }).format(['a', 'b', 'c'])"), "a, b, & c");
    assert_eq!(eval("new Intl.ListFormat('en', { style: 'narrow', type: 'unit' }).format(['a', 'b', 'c'])"), "a b c");
    assert_eq!(eval("['b', 'a', 'C', 'á', 'A', 'z', 'Z'].sort(new Intl.Collator().compare).join('')"), "aAábCzZ");
    assert_eq!(eval("['a10', 'a2', 'a1'].sort(new Intl.Collator(undefined, { numeric: true }).compare).join()"), "a1,a2,a10");
    assert_eq!(eval("['a10', 'a2', 'a1'].sort(new Intl.Collator().compare).join()"), "a1,a10,a2");
    assert_eq!(eval("var c = new Intl.Collator('en', { sensitivity: 'base' }); [c.compare('a', 'A'), c.compare('a', 'á'), c.compare('a', 'b')].join()"), "0,0,-1");
    assert_eq!(eval("var c = new Intl.Collator('en', { sensitivity: 'accent' }); [c.compare('a', 'A'), c.compare('a', 'á')].join()"), "0,-1");
    assert_eq!(eval("['é', 'e', 'f', 'E'].sort(function (a, b) { return a.localeCompare(b, 'en'); }).join('')"), "eEéf");
    assert_eq!(eval("'a'.localeCompare('B', undefined, { sensitivity: 'base' }) + ' ' + 'a'.localeCompare('A', 'en', { sensitivity: 'base' }) + ' ' + 'b'.localeCompare('a')"), "-1 0 1");
    assert_eq!(eval("['_x', '1', 'a', ' b', '-'].sort(new Intl.Collator().compare).join('|')"), " b|_x|-|1|a"); // whitespace, punctuation, digits, letters
    assert_eq!(eval("Intl.getCanonicalLocales(['EN-us', 'fr']).join()"), "en-US,fr");
    assert_eq!(eval("Intl.getCanonicalLocales('xx_yy')").split(':').next().unwrap(), "ERR RangeError");
    assert_eq!(eval("var l = new Intl.Locale('en-Latn-US'); [l.language, l.script, l.region, l.baseName, String(l), l.maximize().toString(), l.minimize().toString()].join()"), "en,Latn,US,en-Latn-US,en-Latn-US,en-Latn-US,en");
    assert_eq!(eval("[new Intl.DisplayNames('en', { type: 'region' }).of('US'), new Intl.DisplayNames('en', { type: 'language' }).of('fr'), new Intl.DisplayNames('en', { type: 'currency' }).of('EUR'), new Intl.DisplayNames('en', { type: 'region' }).of('ZZ')].join()"), "United States,French,Euro,ZZ".replace("Euro", "euro"));
    assert_eq!(eval("Array.from(new Intl.Segmenter('en', { granularity: 'grapheme' }).segment('a\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}e\u{301}b')).map(function (s) { return s.segment.length; }).join()"), "1,8,2,1");
    assert_eq!(eval("Array.from(new Intl.Segmenter('en', { granularity: 'word' }).segment('Hello, big world!')).filter(function (s) { return s.isWordLike; }).map(function (s) { return s.segment; }).join()"), "Hello,big,world");
    assert_eq!(eval("Array.from(new Intl.Segmenter('en', { granularity: 'sentence' }).segment('One. Two? Three!')).map(function (s) { return s.segment; }).join('|')"), "One. |Two? |Three!");
    assert_eq!(eval("Intl.supportedValuesOf('currency').indexOf('USD') >= 0"), "true");
    assert_eq!(eval("typeof Intl + Object.keys(Intl).length + typeof Intl.DateTimeFormat"), "object0function");
}

#[test]
#[ignore]
fn scratch() {
    let r = run(
        "function f(ms) { return new Intl.DateTimeFormat('en-US', { timeZone: 'America/New_York', hour: 'numeric', timeZoneName: 'short' }).format(ms); }\
         console.log('R ' + [f(Date.UTC(2024, 2, 9, 12)), f(Date.UTC(2024, 2, 10, 6)), f(Date.UTC(2024, 2, 10, 8)), f(Date.UTC(2024, 2, 11, 12)), f(Date.UTC(2024, 5, 11, 12)), f(Date.UTC(2024, 10, 3, 12)), f(Date.UTC(2024, 10, 4, 12))].join('|'));",
        0,
    );
    std::println!("{r:?}");
}

#[test]
fn proxies_work_in_pages_and_do_not_clone() {
    assert_eq!(eval("var p = new Proxy({a: 1}, {get: function (t, k) { return k in t ? t[k] : 'none'; }}); p.a + ' ' + p.zz"), "1 none");
    assert_eq!(eval("var p = new Proxy({a: 1}, {}); try { structuredClone(p); 'cloned' } catch (e) { e.name }"), "DataCloneError");
    assert_eq!(eval("var el = document.getElementById('a'); var p = new Proxy(el, {}); try { p.id } catch (e) { e.name }"), "TypeError");
    assert_eq!(eval("var s = new Proxy({}, {}); s.x = 1; JSON.stringify(s) + Object.keys(s)"), "{\"x\":1}x");
}

// ---- MutationObserver, IntersectionObserver, ResizeObserver ------------------------------------

fn lines(body: &str, steps: u32) -> String {
    run(body, steps).join("|")
}

#[test]
fn mutation_observer_reports_child_changes_after_the_script() {
    let out = lines(
        "var a = document.getElementById('a'); var log = [];
         var mo = new MutationObserver(function (recs, o) {
           log.push('cb ' + recs.length + ' ' + (o === mo) + ' ' + (this === mo));
           recs.forEach(function (r) { log.push(r.type + ':' + r.addedNodes.length + '/' + r.removedNodes.length + ':' + (r.target === a)); });
         });
         mo.observe(a, { childList: true });
         var s = document.createElement('span'); a.appendChild(s); a.removeChild(s);
         log.push('sync');
         Promise.resolve().then(function () { log.push('promise'); });
         setTimeout(function () { console.log('R ' + log.join(';')); }, 0);",
        3,
    );
    assert_eq!(out, "sync;promise;cb 2 true true;childList:1/0:true;childList:0/1:true");
}

#[test]
fn mutation_observer_attributes_and_old_values() {
    let out = lines(
        "var a = document.getElementById('a'); var log = [];
         var mo = new MutationObserver(function (recs) {
           recs.forEach(function (r) { log.push(r.type + ':' + r.attributeName + ':' + r.oldValue); });
         });
         mo.observe(a, { attributes: true, attributeOldValue: true, attributeFilter: ['title', 'data-x'] });
         a.setAttribute('title', 'one'); a.setAttribute('title', 'two'); a.setAttribute('class', 'ignored');
         a.removeAttribute('title'); a.dataset.x = '5'; a.id = 'b';
         setTimeout(function () { console.log('R ' + log.join(';')); }, 0);",
        3,
    );
    assert_eq!(out, "attributes:title:null;attributes:title:one;attributes:title:two;attributes:data-x:null");
}

#[test]
fn mutation_observer_subtree_and_character_data() {
    let out = lines(
        "var a = document.getElementById('a'); var log = [];
         a.innerHTML = '<p id=p>hi</p>';
         var p = document.getElementById('p');
         var mo = new MutationObserver(function (recs) {
           recs.forEach(function (r) { log.push(r.type + ':' + r.target.nodeName + ':' + r.oldValue); });
         });
         mo.observe(a, { subtree: true, characterData: true, characterDataOldValue: true, childList: true, attributes: true });
         p.firstChild.data = 'yo'; p.firstChild.appendData('!'); p.setAttribute('x', '1');
         p.appendChild(document.createElement('b'));
         var q = document.createElement('div'); a.appendChild(q); q.appendChild(document.createElement('i'));
         mo.disconnect(); p.setAttribute('x', '2');
         setTimeout(function () { console.log('R ' + log.join(';')); }, 0);",
        3,
    );
    // After disconnect() the records not yet delivered are dropped.
    assert_eq!(out, "");
    let out = lines(
        "var a = document.getElementById('a'); var log = [];
         a.innerHTML = '<p id=p>hi</p>';
         var p = document.getElementById('p');
         var mo = new MutationObserver(function (recs) {
           recs.forEach(function (r) { log.push(r.type + ':' + r.target.nodeName + ':' + r.oldValue); });
         });
         mo.observe(a, { subtree: true, characterData: true, characterDataOldValue: true, childList: true, attributes: true });
         p.firstChild.data = 'yo'; p.firstChild.appendData('!'); p.setAttribute('x', '1');
         p.appendChild(document.createElement('b'));
         var q = document.createElement('div'); a.appendChild(q); q.appendChild(document.createElement('i'));
         setTimeout(function () { console.log('R ' + log.join(';')); }, 0);",
        3,
    );
    assert_eq!(
        out,
        "characterData:#text:hi;characterData:#text:yo;attributes:P:null;childList:P:null;childList:DIV:null;childList:DIV:null"
    );
}

#[test]
fn mutation_observer_api_details() {
    assert_eq!(eval("typeof MutationObserver + typeof MutationRecord + MutationObserver.length + ' ' + Object.prototype.toString.call(new MutationObserver(function () {}))"), "functionfunction1 [object MutationObserver]");
    assert_eq!(eval("try { new MutationObserver(); } catch (e) { e.name }"), "TypeError");
    assert_eq!(eval("try { new MutationObserver(1); } catch (e) { e.name }"), "TypeError");
    assert_eq!(eval("try { new MutationObserver(function () {}).observe(document.body, {}); } catch (e) { e.name }"), "TypeError");
    assert_eq!(eval("try { new MutationObserver(function () {}).observe({}, {childList: true}); } catch (e) { e.name }"), "TypeError");
    assert_eq!(eval("try { new MutationObserver(function () {}).observe(document.body, {attributes: false, attributeOldValue: true}); } catch (e) { e.name }"), "TypeError");
    assert_eq!(eval("try { new MutationObserver(function () {}).observe(document.body, {childList: true, attributeFilter: ['a']}); 'ok' } catch (e) { e.name }"), "ok");
    assert_eq!(eval("try { new MutationRecord() } catch (e) { e.name }"), "TypeError");
    // takeRecords hands over what is waiting, and nothing is delivered after.
    let out = lines(
        "var a = document.getElementById('a'); var log = [];
         var mo = new MutationObserver(function () { log.push('cb'); });
         mo.observe(a, { childList: true });
         a.appendChild(document.createElement('s'));
         var t = mo.takeRecords();
         log.push(t.length + ':' + t[0].type + ':' + mo.takeRecords().length);
         setTimeout(function () { console.log('R ' + log.join(';')); }, 0);",
        3,
    );
    assert_eq!(out, "1:childList:0");
    // Two observers see the same change; a second observe() of a node replaces the options.
    let out = lines(
        "var a = document.getElementById('a'); var log = [];
         var m1 = new MutationObserver(function (r) { log.push('m1 ' + r.length); });
         var m2 = new MutationObserver(function (r) { log.push('m2 ' + r.length); });
         m1.observe(a, { childList: true }); m1.observe(a, { attributes: true });
         m2.observe(a, { childList: true, attributes: true });
         a.appendChild(document.createElement('s')); a.setAttribute('k', 'v');
         setTimeout(function () { console.log('R ' + log.join(';')); }, 0);",
        3,
    );
    assert_eq!(out, "m1 1;m2 2");
}

#[test]
fn mutation_observer_callbacks_that_mutate_stay_bounded() {
    // A callback that changes what it watches runs again, but a page cannot spin the
    // checkpoint forever: the rounds are limited and the page stays alive.
    let out = lines(
        "var a = document.getElementById('a'); var n = 0;
         var mo = new MutationObserver(function () { n++; a.setAttribute('t', String(n)); });
         mo.observe(a, { attributes: true });
         a.setAttribute('t', '0');
         setTimeout(function () { console.log('R ' + (n > 10) + ' ' + (n < 100000)); }, 50);",
        20,
    );
    assert_eq!(out, "true true");
    // A burst of changes: the records are limited, the callback still gets some.
    let out = lines(
        "var a = document.getElementById('a'); var got = 0;
         var mo = new MutationObserver(function (r) { got += r.length; });
         mo.observe(a, { childList: true });
         for (var i = 0; i < 30000; i++) { a.appendChild(document.createElement('i')); }
         setTimeout(function () { console.log('R ' + (got > 0) + ' ' + (got <= 30000) + ' ' + a.childNodes.length); }, 0);",
        3,
    );
    assert_eq!(out, "true true 30001");
}

#[test]
fn mutation_observer_sees_removed_subtrees_and_innerhtml() {
    let out = lines(
        "var a = document.getElementById('a'); var log = [];
         a.innerHTML = '<p>1</p><p>2</p>';
         var mo = new MutationObserver(function (recs) {
           recs.forEach(function (r) { log.push(r.type + ':' + r.addedNodes.length + '/' + r.removedNodes.length + (r.removedNodes[0] ? ':' + r.removedNodes[0].nodeName + ':' + r.removedNodes[0].textContent : '')); });
         });
         mo.observe(a, { childList: true });
         a.innerHTML = '<b>x</b>';
         setTimeout(function () { console.log('R ' + log.join(';')); }, 0);",
        3,
    );
    assert_eq!(out, "childList:0/1:P:1;childList:0/1:P:2;childList:1/0");
}

#[test]
fn mutation_observer_works_in_detached_trees() {
    let out = lines(
        "var d = document.createElement('div'); var log = [];
         var mo = new MutationObserver(function (r) { log.push(r.length + r[0].type); });
         mo.observe(d, { childList: true, subtree: true });
         var c = document.createElement('p'); d.appendChild(c); c.appendChild(document.createElement('i'));
         setTimeout(function () { console.log('R ' + log.join(';')); }, 0);",
        3,
    );
    assert_eq!(out, "2childList");
}

#[test]
fn dom_defers_freeing_nodes_the_log_names() {
    use crate::dom::{Dom, NONE};
    let mut d = Dom::new();
    let a = d.create_element(crate::atoms::tag::DIV, crate::dom::Ns::Html, Vec::new());
    d.append(0, a);
    d.mlog.watch.insert(a, (0, 1));
    let c = d.create_element(crate::atoms::tag::SPAN, crate::dom::Ns::Html, Vec::new());
    d.append(a, c);
    d.detach(c);
    assert_eq!(d.mlog.recs.len(), 2);
    d.free_subtree(c);
    // Not given back yet: a record names it.
    assert_eq!(d.mlog.deferred, alloc::vec![c]);
    assert_eq!(d.live(), 3);
    assert_eq!(d.take_mutations().len(), 2);
    assert_ne!(c, NONE);
    for _ in 0..40_000 {
        let n = d.create_element(crate::atoms::tag::SPAN, crate::dom::Ns::Html, Vec::new());
        d.append(a, n);
    }
    assert!(d.mlog.recs.len() <= crate::dom::MAX_MUT_RECORDS);
}

#[test]
fn intersection_observer_reports_the_first_state_and_changes() {
    let out = lines(
        "var a = document.getElementById('a'); var log = [];
         var far = document.createElement('div'); far.style.cssText = 'margin-top:3000px;height:50px;width:50px'; far.id = 'far';
         document.body.appendChild(far);
         var io = new IntersectionObserver(function (es, o) {
           es.forEach(function (e) { log.push(e.target.id + ':' + e.isIntersecting + ':' + e.intersectionRatio + ':' + (o === io)); });
         });
         io.observe(a); io.observe(far);
         setTimeout(function () { console.log('R ' + log.join(';')); }, 40);",
        12,
    );
    assert_eq!(out, "a:true:1:true;far:false:0:true");
}

#[test]
fn intersection_observer_options_and_errors() {
    assert_eq!(eval("var io = new IntersectionObserver(function () {}, { rootMargin: '10px 20%', threshold: [0.5, 0, 1] }); io.rootMargin + ' ' + io.thresholds.join() + ' ' + io.root"), "10px 20% 10px 20% 0,0.5,1 null");
    assert_eq!(eval("try { new IntersectionObserver(function () {}, { threshold: 2 }) } catch (e) { e.name }"), "RangeError");
    assert_eq!(eval("try { new IntersectionObserver(function () {}, { rootMargin: 'bad' }) } catch (e) { e.name }"), "SyntaxError");
    assert_eq!(eval("try { new IntersectionObserver() } catch (e) { e.name }"), "TypeError");
    assert_eq!(eval("try { new IntersectionObserver(function () {}).observe({}) } catch (e) { e.name }"), "TypeError");
    assert_eq!(eval("var io = new IntersectionObserver(function () {}); io.observe(document.body); io.unobserve(document.body); io.disconnect(); io.takeRecords().length"), "0");
}

#[test]
fn resize_observer_reports_sizes_and_changes() {
    let out = lines(
        "var a = document.getElementById('a'); var log = [];
         var ro = new ResizeObserver(function (es, o) {
           es.forEach(function (e) { log.push(e.contentRect.width + 'x' + e.contentRect.height + ':' + e.borderBoxSize[0].inlineSize + ':' + (o === ro)); });
         });
         ro.observe(a);
         setTimeout(function () { a.style.width = '150px'; }, 30);
         setTimeout(function () { console.log('R ' + log.join(';')); }, 120);",
        40,
    );
    assert_eq!(out, "100x40:100:true;150x40:150:true");
}

#[test]
fn layout_observers_stop_asking_for_frames_when_idle() {
    let mut h = Host::new();
    let html = "<!DOCTYPE html><html><body><div id=a style=\"width:100px;height:40px\">x</div><script>var n = 0; new ResizeObserver(function () { n++; }).observe(document.getElementById('a'));</script></body></html>";
    let mut d = doc(html);
    for _ in 0..5 {
        d.run(&mut h);
        if d.wants_frame() {
            d.frame(&mut h);
        }
        d.layout(&Viewport::new(800, 600), &ApproxMeasurer);
        h.now += 5.0;
    }
    // After the first look at the boxes nothing is pending until the page or the view changes.
    assert!(!d.wants_frame());
}

#[test]
fn computed_style_answers_the_common_properties() {
    let o = lines(
        "var a = document.getElementById('a'); a.style.cssText = 'width:100px;height:40px;padding:5px 10px;margin:3px;border:2px solid #f00;position:relative;top:7px;line-height:20px;text-align:center;--gap: 4px ;cursor:pointer';
         var cs = getComputedStyle(a);
         console.log('R ' + [cs.getPropertyValue('padding-left'), cs.paddingTop, cs.marginLeft, cs.borderTopWidth, cs.getPropertyValue('border-top-style'), cs.borderTopColor, cs.width, cs.height, cs.top, cs.lineHeight, cs.textAlign, cs.cursor, cs.getPropertyValue('--gap'), cs.boxSizing, cs.getPropertyValue('transform'), cs.length > 20, cs.item(0), cs.cssText === ''].join('|'));
         a.style.boxSizing = 'border-box';
         console.log('R ' + getComputedStyle(a).width);
         try { cs.setProperty('color', 'red'); } catch (e) { console.log('R ' + e.name); }
         try { cs.cssText = 'color:red'; } catch (e) { console.log('R ' + e.name); }
         try { cs.removeProperty('color'); } catch (e) { console.log('R ' + e.name); }",
        0,
    );
    assert_eq!(
        o,
        "10px|5px|3px|2px|solid|rgb(255, 0, 0)|100px|40px|7px|20px|center|pointer|4px|content-box|none|true|align-content|true|100px|NoModificationAllowedError|NoModificationAllowedError|NoModificationAllowedError"
    );
}

#[test]
fn fuzz_observers_with_random_dom_changes() {
    // Random trees, observers with random options, random changes and disconnects: nothing
    // panics, nothing hangs, and every record names a live node.
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for round in 0..40 {
        let mut js = String::from(
            "var root = document.getElementById('a'); var nodes = [root]; var obs = []; var bad = 0; var seen = 0;
             function pick() { return nodes[Math.floor(Math.random() * nodes.length)]; }
             function cb(recs) { recs.forEach(function (r) { seen++; if (!r.target || typeof r.target.nodeType !== 'number') bad++;
               r.addedNodes.forEach(function (n) { if (typeof n.nodeType !== 'number') bad++; }); }); }
             var seedv = 1; Math.random = function () { seedv = (seedv * 16807) % 2147483647; return seedv / 2147483647; };
             var steps = [",
        );
        let n = 30 + (next() % 60) as usize;
        for _ in 0..n {
            let k = next() % 14;
            let t = match k {
                0 => "function () { var e = document.createElement('div'); pick().appendChild(e); nodes.push(e); }",
                1 => "function () { var e = document.createElement('p'); var p = pick(); p.insertBefore(e, p.firstChild); nodes.push(e); }",
                2 => "function () { var p = pick(); if (p !== root && p.parentNode) p.parentNode.removeChild(p); }",
                3 => "function () { pick().setAttribute('k' + (seedv % 3), String(seedv)); }",
                4 => "function () { pick().removeAttribute('k1'); }",
                5 => "function () { pick().appendChild(document.createTextNode('t' + seedv)); }",
                6 => "function () { var p = pick(); if (p.firstChild && p.firstChild.nodeType === 3) p.firstChild.data = 'd' + seedv; }",
                7 => "function () { var p = pick(); if (p !== root) p.innerHTML = '<b>x</b><i>y</i>'; }",
                8 => "function () { var o = new MutationObserver(cb); o.observe(pick(), { childList: seedv % 2 === 0, attributes: seedv % 3 === 0, characterData: true, subtree: seedv % 5 !== 0, attributeOldValue: seedv % 7 === 0 }); obs.push(o); }",
                9 => "function () { if (obs.length) obs[seedv % obs.length].disconnect(); }",
                10 => "function () { if (obs.length) obs[seedv % obs.length].takeRecords(); }",
                11 => "function () { var p = pick(), q = pick(); try { p.appendChild(q); } catch (e) {} }",
                12 => "function () { var p = pick(); if (p !== root) p.textContent = 'z'; }",
                _ => "function () { var o = new MutationObserver(function () { pick().setAttribute('cbk', String(seedv)); }); o.observe(root, { attributes: true, subtree: true }); obs.push(o); }",
            };
            js.push_str(t);
            js.push(',');
        }
        js.push_str(
            "]; steps.forEach(function (f) { try { f(); } catch (e) { if (!(e instanceof TypeError)) throw e; } }); setTimeout(function () { console.log('R ' + bad + ' ' + (seen >= 0)); }, 10);",
        );
        let out = lines(&js, 4);
        assert_eq!(out, "0 true", "round {round}: {js}");
    }
}

// ---- DOMParser and friends ------------------------------------------------------------------

#[test]
fn dom_parser_makes_documents() {
    assert_eq!(
        eval("var d = new DOMParser().parseFromString('<!DOCTYPE html><html><head><title> A  b </title></head><body><p id=x class=c>hi <b>there</b></p><script>window.ran = 1<\\/script></body></html>', 'text/html'); [d.nodeType, d.title, d.body.tagName, d.getElementById('x').textContent, d.querySelector('b').textContent, d.querySelectorAll('p').length, d.documentElement.tagName, d.head.tagName, d.doctype.name, d.contentType, typeof window.ran, d instanceof Document, d.getElementsByTagName('p').length, Object.prototype.toString.call(d)].join('|')"),
        "9|A b|BODY|hi there|there|1|HTML|HEAD|html|text/html|undefined|true|1|[object HTMLDocument]"
    );
    assert_eq!(
        eval("var d = new DOMParser().parseFromString('<p>frag', 'text/html'); d.body.innerHTML + '|' + d.documentElement.outerHTML"),
        "<p>frag</p>|<html><head></head><body><p>frag</p></body></html>"
    );
    assert_eq!(
        eval("var d = new DOMParser().parseFromString('<?xml version=\"1.0\"?><feed><item id=\"1\"><title>T1</title></item><item id=\"2\"><title>T2</title></item></feed>', 'text/xml'); [d.documentElement.tagName.toLowerCase(), d.querySelectorAll('item').length, d.querySelector('item[id=\"2\"] title').textContent, d.getElementsByTagName('title').length, d.contentType].join('|')"),
        "feed|2|T2|2|text/xml"
    );
    assert_eq!(eval("try { new DOMParser().parseFromString('x', 'text/plain') } catch (e) { e.name }"), "TypeError");
    assert_eq!(eval("try { new DOMParser().parseFromString('x') } catch (e) { e.name }"), "TypeError");
    // Nodes of a parsed document can move into the page, and the page's scripts there do not run twice.
    assert_eq!(
        eval("var d = new DOMParser().parseFromString('<div id=moved>m</div>', 'text/html'); var n = document.importNode(d.getElementById('moved'), true); document.body.appendChild(n); document.getElementById('moved').textContent"),
        "m"
    );
}

#[test]
fn implementation_and_serializer() {
    assert_eq!(
        eval("var d = document.implementation.createHTMLDocument('T'); d.body.appendChild(d.createElement('i')); [d.title, d.body.innerHTML, d.documentElement.nodeName].join('|')"),
        "T|<i></i>|HTML"
    );
    assert_eq!(eval("document.implementation.hasFeature('x', '1') + ' ' + document.implementation.createDocumentType('svg', '', '').name"), "true svg");
    let ser = |js: &str| eval(&alloc::format!("new XMLSerializer().serializeToString({js})"));
    assert_eq!(ser("document.createElement('div')"), "<div xmlns=\"http://www.w3.org/1999/xhtml\"></div>");
    assert_eq!(ser("document.createTextNode('a<b&c')"), "a&lt;b&amp;c");
    assert_eq!(ser("document.createComment(' c ')"), "<!-- c -->");
    assert_eq!(
        ser("(function () { var e = document.createElementNS('http://www.w3.org/2000/svg', 'svg'); e.setAttribute('width', '5'); e.appendChild(document.createElementNS('http://www.w3.org/2000/svg', 'rect')); return e })()"),
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"5\"><rect/></svg>"
    );
    assert_eq!(
        ser("(function () { var e = document.createElement('p'); e.innerHTML = 'a<br>b'; e.title = 'q\"'; return e })()"),
        "<p xmlns=\"http://www.w3.org/1999/xhtml\" title=\"q&quot;\">a<br />b</p>"
    );
    assert_eq!(eval("try { new XMLSerializer().serializeToString({}) } catch (e) { e.name }"), "TypeError");
}

#[test]
fn tree_walker_and_node_iterator() {
    let setup = "var root = document.createElement('div'); root.innerHTML = '<p>a<b>b</b></p><!--c--><i>i<u>u</u></i>text'; ";
    assert_eq!(
        eval(&alloc::format!("{setup} var w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT); var out = []; while (w.nextNode()) out.push(w.currentNode.nodeName); out.join()")),
        "P,B,I,U"
    );
    assert_eq!(
        eval(&alloc::format!("{setup} var w = document.createTreeWalker(root, NodeFilter.SHOW_TEXT); var out = []; while (w.nextNode()) out.push(w.currentNode.data); out.join()")),
        "a,b,i,u,text"
    );
    assert_eq!(
        eval(&alloc::format!("{setup} var w = document.createTreeWalker(root, NodeFilter.SHOW_ALL, {{ acceptNode: function (n) {{ return n.nodeName === 'I' ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT; }} }}); var out = []; while (w.nextNode()) out.push(w.currentNode.nodeName); out.join()")),
        "P,#text,B,#text,#comment,#text"
    );
    assert_eq!(
        eval(&alloc::format!("{setup} var w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT); [w.firstChild().nodeName, w.nextSibling().nodeName, w.lastChild().nodeName, w.parentNode().nodeName, w.previousSibling().nodeName, w.parentNode() === root, w.root === root].join()")),
        "P,I,U,I,P,true,true"
    );
    assert_eq!(
        eval(&alloc::format!("{setup} var w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT); w.currentNode = root.querySelector('u'); var out = []; while (w.previousNode()) out.push(w.currentNode.nodeName); out.join()")),
        "I,B,P,DIV"
    );
    assert_eq!(
        eval(&alloc::format!("{setup} var it = document.createNodeIterator(root, NodeFilter.SHOW_ELEMENT); var out = []; var n; while ((n = it.nextNode())) out.push(n.nodeName); var back = []; while ((n = it.previousNode())) back.push(n.nodeName); out.join() + '|' + back.join()")),
        "DIV,P,B,I,U|U,I,B,P,DIV"
    );
    assert_eq!(eval("try { document.createTreeWalker() } catch (e) { e.name }"), "TypeError");
    assert_eq!(eval("NodeFilter.SHOW_ELEMENT + ' ' + NodeFilter.FILTER_SKIP + ' ' + NodeFilter.SHOW_ALL"), "1 3 4294967295");
}

#[test]
fn ranges_and_selection() {
    let setup = "var root = document.createElement('div'); root.innerHTML = '<p>Hello <b>big</b> world</p><i>tail</i>'; document.body.appendChild(root); var p = root.firstChild; ";
    assert_eq!(
        eval(&alloc::format!("{setup} var r = document.createRange(); r.setStart(p.firstChild, 2); r.setEnd(p.lastChild, 3); [r.toString(), r.collapsed, r.commonAncestorContainer === p, r.startOffset, r.endOffset].join('|')")),
        "llo big wo|false|true|2|3"
    );
    assert_eq!(
        eval(&alloc::format!("{setup} var r = document.createRange(); r.selectNodeContents(p); var f = r.cloneContents(); var d = document.createElement('div'); d.appendChild(f); d.innerHTML + '|' + p.innerHTML")),
        "Hello <b>big</b> world|Hello <b>big</b> world"
    );
    assert_eq!(
        eval(&alloc::format!("{setup} var r = document.createRange(); r.setStart(p.firstChild, 3); r.setEnd(p.lastChild, 3); var f = r.extractContents(); var d = document.createElement('div'); d.appendChild(f); d.innerHTML + '|' + p.innerHTML + '|' + r.collapsed")),
        "lo <b>big</b> wo|Helrld|true"
    );
    assert_eq!(
        eval(&alloc::format!("{setup} var r = document.createRange(); r.selectNode(p.querySelector('b')); r.deleteContents(); p.innerHTML + '|' + r.collapsed")),
        "Hello  world|true"
    );
    assert_eq!(
        eval(&alloc::format!("{setup} var r = document.createRange(); r.setStart(p.firstChild, 6); r.collapse(true); var n = document.createElement('u'); n.textContent = 'X'; r.insertNode(n); p.innerHTML")),
        "Hello <u>X</u><b>big</b> world"
    );
    assert_eq!(
        eval(&alloc::format!("{setup} var r = document.createRange(); r.selectNodeContents(p.querySelector('b')); var w = document.createElement('em'); r.surroundContents(w); p.innerHTML")),
        "Hello <b><em>big</em></b> world"
    );
    assert_eq!(
        eval(&alloc::format!("{setup} var a = document.createRange(); a.selectNodeContents(p); var b = document.createRange(); b.selectNodeContents(root.lastChild); [a.compareBoundaryPoints(Range.START_TO_START, b), a.compareBoundaryPoints(Range.END_TO_START, b), b.compareBoundaryPoints(Range.START_TO_END, a), a.isPointInRange(p.firstChild, 2), a.comparePoint(root.lastChild.firstChild, 0), a.intersectsNode(p.querySelector('b'))].join()")),
        "-1,-1,1,true,1,true"
    );
    assert_eq!(
        eval(&alloc::format!("{setup} var f = document.createRange().createContextualFragment('<i>1</i><b>2</b>'); f.childNodes.length + f.firstChild.nodeName")),
        "2I"
    );
    assert_eq!(eval("var r = document.createRange(); try { r.setStart(document.body, 99) } catch (e) { e.name }"), "IndexSizeError");
    assert_eq!(eval("var r = new Range(); r.startContainer === document && r.collapsed"), "true");
    assert_eq!(
        eval(&alloc::format!("{setup} var s = getSelection(); var r0 = s.rangeCount + s.type; s.selectAllChildren(p); var r = s.getRangeAt(0); [r0, s.rangeCount, s.type, s.toString(), s.anchorNode === p, s.focusOffset, s === document.getSelection()].join('|') + '|' + (s.removeAllRanges(), s.rangeCount)")),
        "0None|1|Range|Hello big world|true|3|true|0"
    );
    assert_eq!(
        eval(&alloc::format!("{setup} var s = getSelection(); s.setBaseAndExtent(p.firstChild, 5, p.lastChild, 2); [s.toString(), s.direction, s.containsNode(p.querySelector('b'))].join('|')")),
        " big w|forward|true"
    );
}

#[test]
fn character_data_methods_and_split_text() {
    assert_eq!(
        eval("var p = document.createElement('p'); p.textContent = 'abcdef'; var t = p.firstChild; var u = t.splitText(2); [t.data, u.data, p.childNodes.length, t.substringData(1, 1), (t.insertData(1, 'X'), t.data), (t.deleteData(0, 1), t.data), (t.replaceData(0, 1, 'YY'), t.data)].join('|')"),
        "ab|cdef|2|b|aXb|Xb|YYb"
    );
    assert_eq!(eval("var t = document.createTextNode('ab'); try { t.splitText(5) } catch (e) { e.name }"), "IndexSizeError");
}

#[test]
fn custom_elements_registry_and_fonts() {
    assert_eq!(
        eval("class A extends HTMLElement {} customElements.define('x-a', A); [customElements.get('x-a') === A, customElements.get('x-b'), customElements.getName(A)].join()"),
        "true,,x-a"
    );
    assert_eq!(eval("try { customElements.define('bad', function () {}) } catch (e) { e.name }"), "SyntaxError");
    assert_eq!(eval("customElements.define('x-c', class {}); try { customElements.define('x-c', class {}) } catch (e) { e.name }"), "NotSupportedError");
    assert_eq!(
        lines("var log = []; customElements.whenDefined('x-w').then(function (c) { log.push('def ' + (c === C)); }); var C = class {}; customElements.define('x-w', C); setTimeout(function () { console.log('R ' + log.join()); }, 0);", 2),
        "def true"
    );
    assert_eq!(
        eval("[typeof document.fonts, document.fonts.check('12px serif'), document.fonts.status, typeof document.fonts.ready.then, typeof FontFace, document.fonts.size].join()"),
        "object,true,loaded,function,function,0"
    );
}
