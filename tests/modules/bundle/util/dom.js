// A tiny hyperscript.
export function h(tag, props, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(props || {})) {
    if (k.startsWith('on')) el.addEventListener(k.slice(2).toLowerCase(), v);
    else if (k === 'class') el.className = v;
    else el.setAttribute(k, v);
  }
  for (const c of children.flat()) el.append(c instanceof Node ? c : String(c));
  return el;
}

export function mount(root, nodes) {
  root.replaceChildren(...nodes);
}
