// The same for modules.
export function report(group, name, value) {
  console.log('MODTEST ' + group + ' ' + name + ' ' + value);
}
export function errName(e) {
  return (e && e.name ? e.name : 'Error') + ': ' + (e && e.message ? e.message : e);
}
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
