// a.js and b.js import each other.
import { name } from './a.js';
export const b = 'b';
export function fromA() { return 'cycle sees a=' + name; }
