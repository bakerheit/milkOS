import { h } from '../util/dom.js';
import { plural } from '../util/format.js';

export function Header({ cart }) {
  const count = Object.values(cart).reduce((a, b) => a + b, 0);
  return h('div', { class: 'header' },
    h('span', { class: 'badge' }, plural(count, 'item')),
    h('h1', {}, 'The cheese shop'),
    h('small', {}, 'built from six modules and a JSON file'));
}
