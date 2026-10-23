// Shared by the modules-test pages that need a global (classic) reporter: a line the test
// script reads from the serial log ("[browser] console log: MODTEST group name result").
function report(group, name, value) {
  console.log('MODTEST ' + group + ' ' + name + ' ' + value);
}
function errName(e) {
  return (e && e.name ? e.name : 'Error') + ': ' + (e && e.message ? e.message : e);
}
