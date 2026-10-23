import { h } from '../util/dom.js';
import { money, plural } from '../util/format.js';

export function Cart({ cheeses, cart }) {
  const lines = cheeses.filter((c) => cart[c.name]);
  const total = lines.reduce((sum, c) => sum + c.price * cart[c.name], 0);
  return h('div', { class: 'cart' },
    h('b', {}, 'Your basket'),
    lines.length === 0
      ? h('div', { class: 'empty' }, 'Nothing yet.')
      : lines.map((c) => h('div', { class: 'line' }, `${plural(cart[c.name], c.name)} - ${money(c.price * cart[c.name])}`)),
    h('div', { class: 'total' }, `Total: ${money(total)}`));
}
