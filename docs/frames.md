# Frames in the web engine

`<iframe>` support in `user/webengine`. The interface is the "Frames" section at the end of
`user/webengine/src/api.rs` (and its CHANGES entry); this page is the model and the reasons.

## The model

A page is a tree of frames. The top document is `FrameId::TOP`; every `<iframe>` element in any
document of the page has a *slot* in one table owned by the top document, with an id that is never
used twice. A slot holds at most one document, which the host attached; the engine never fetches.

```
frame_slots()  →  the host loads what a slot wants (srcdoc, src, or about:blank)
attach_frame(id, doc, Some(generation))      detach_frame(id)      take_closed_frames()
with_frame(id, |doc| doc.run(host) ...)       frame_doc(id) / frame_doc_mut(id)
layout(&vp, m)  →  one display list, the frames' content inside it
hit_test, scroll_frames_at, reveal, focus_order, find, text  →  across frames
```

Why one table in the top document and not documents holding their children: a document can be
built and handed over without knowing where it will sit, ids are unique without coordination,
replacing a frame's document drops its frames' slots in one place, and the caps (32 slots, depth 5)
are one counter each. A child document's own `<iframe>`s are slots of the top table whose parent
is the child.

## What the engine does

- **Elements**: `<iframe>` is a replaced inline-block, 300 x 150 by default, `width` / `height`
  attributes and CSS, the UA's 2px inset border (`frameborder=0` or `no` removes it).
  `srcdoc`, `src`, `name`, `sandbox` (parsed into `Sandbox`), `loading`, `allow`,
  `allowfullscreen`, `referrerpolicy` and `scrolling` are read into the slot; the host decides
  what they mean. `<frame>` / `<frameset>` and `<object>` / `<embed>` documents are not supported.
- **Layout**: each frame's document is laid out in a viewport the size of the slot's content box,
  at the parent's zoom and colour scheme. A frame with no box (`display: none`) keeps its slot
  and is not laid out. A frame paints its own canvas over its slot (opaque).
- **Paint**: the child's entries are spliced into the parent's list where the `<iframe>` is
  painted, so a positioned box above a frame covers it. Every entry is clipped to the slot (and to
  whatever clips the `<iframe>` itself), moved by the slot's position minus the frame's scroll, and
  only the frame's visible window is copied (a frame with a huge document costs what it shows).
  `Entry::frame` says whose entry it is: images and controls belong to a document.
  A frame's `position: fixed` boxes are fixed to the frame's view.
- **Pointer**: `hit_test` goes down through frames to the innermost one under the point and says
  where the point is in that frame's viewport. A box that is painted above a frame gets the
  pointer, not the frame; the iframe's own border and padding are the element's.
- **Scrolling**: per frame offsets kept by the engine, clamped to the content (`scrolling=no`
  pins them). The wheel scrolls the innermost frame under the pointer, passes what is left to its
  parent, and returns what the page should scroll by. `reveal` scrolls a frame and the frames
  above it to show a rectangle (find, fragments, focus, `scrollIntoView`). An offset or a fragment
  asked for before the frame's first layout (a restored position, `page.html#part`) is kept and
  applied at that layout.
- **Focus**: each document has its own; the table keeps which frame has the keyboard;
  `focus_order` is the Tab order across all frames, a frame's controls at its `<iframe>`.
- **Find and text**: matches in frames come in document order with the frame they are in, the
  rectangles as they are drawn (cut to the slot) and in the frame's own space, for `reveal`.
- **Caps** (`Limits`): `max_frames` (32 slots on a page; the rest of the elements get none, with a
  console note) and `max_frame_depth` (5; deeper slots are `TooDeep` and take no document).
  Attribute copies are bounded (`srcdoc` 1 MB, `name` 64 KB).

## Scripts

Each frame is its own realm with its own host. What the engine gives the bindings:

- `window.name` per frame (the element's `name` at first, kept across navigations),
  `Document::frames_named` for `target=`.
- `window.postMessage` queues an `OutMessage` (data as JSON text, the `targetOrigin` as given);
  the host checks the origin, finds the target frame and calls `deliver_message`, which fires a
  `MessageEvent` (`data`, `origin`, `source` through `script::frames::window_proxy`).
- Where a window sits (`script::frames::own_info`, `child_frames`, `frame_of_element`,
  `frame_element`), `with_peer` and `in_peer` (the same, then an entry of that realm): from a native of one realm, run a closure on another frame's
  document with that frame's host (`ScriptHost::frame_host`). `Document::with_frame` arms it. A
  frame that is running (it called into another that now calls back) is busy and refused.

## What the host still does

Fetching (cookies, redirects, MIME, error pages inside the frame), `X-Frame-Options` and CSP
`frame-ancestors`, mixed content, `sandbox` enforcement, navigation and history, per-frame
`ScriptHost`s and script budgets, scrollbars, cursor, selection, and which frame gets keys.

## Tests

`scripts/webengine-test.sh` runs them (`user/webengine/src/tests/frames.rs`, `framepages.rs`):
slots and caps, attach / detach / replace, layout and clipping, nested frames, scrolling,
`position: fixed`, flex / table / float / clipped parents, hit testing, wheel chaining, `reveal`,
focus order, find and text, `postMessage`, `window.name`, same-origin reach (`with_peer`), pages
that script frames in and out, extreme coordinates, frame bombs, golden display lists
(`tests/golden/frames-*.txt`, `WEBENGINE_BLESS=1` to update) and a fuzz of random nested
structures (`WEBENGINE_FUZZ=N` for more). `WEBENGINE_SHOTS=dir` writes screenshots of the fixture
pages (`tests/fixtures/frames/`).
