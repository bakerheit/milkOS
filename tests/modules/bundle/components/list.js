import { h } from '../util/dom.js';
import { money } from '../util/format.js';

export function CheeseList({ cheeses }, onAdd) {
  return h('div', { class: 'list' }, cheeses.map(({ name, origin, price, aged }) =>
    h('div', { class: 'card' },
      h('span', { class: 'price' }, money(price)),
      h('b', {}, name),
      h('br'),
      `${origin}, aged ${aged} months`,
      h('div', { class: 'age', style: `width: ${Math.min(aged * 12, 300)}px` }),
      h('button', { onClick: () => onAdd(name) }, 'Add'))));
}
