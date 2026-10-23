export const money = (n) => `$${n.toFixed(2)}`;
export const plural = (n, one, many = one + 's') => `${n} ${n === 1 ? one : many}`;
