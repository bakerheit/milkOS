// The entry of a small app written as modules, the way a bundler leaves it for the browser.
import { createStore } from './store.js';
import { mount } from './util/dom.js';
import { Header } from './components/header.js';
import { CheeseList } from './components/list.js';
import { Cart } from './components/cart.js';
import cheeses from './data/cheeses.json' with { type: 'json' };
import { report } from '/report.js';

const store = createStore({ cheeses, cart: {} });
store.add = (name) => store.set((s) => ({ ...s, cart: { ...s.cart, [name]: (s.cart[name] || 0) + 1 } }));

const root = document.getElementById('app');
const view = (state) => [Header(state), (() => {
  const page = document.createElement('div');
  page.className = 'page';
  page.append(CheeseList(state, store.add), Cart(state));
  return page;
})()];

mount(root, view(store.get()));
store.subscribe((state) => mount(root, view(state)));

report('bundle', 'rendered', document.querySelectorAll('.card').length + ' cards, ' + (document.querySelector('.badge') || {}).textContent);

// Later, "the user" adds two cheeses.
setTimeout(() => {
  const buttons = document.querySelectorAll('.card button');
  buttons[0].click();
  buttons[2].click();
  buttons[0].click();
  report('bundle', 'after-clicks', document.querySelector('.badge').textContent + ' | ' + document.querySelector('.total').textContent);
}, 1500);
