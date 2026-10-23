// Shared by the fetch-test pages: report a result the test script can read from the serial
// line ("[browser] console log: FETCHTEST group name result").
function report(group, name, value) {
  console.log('FETCHTEST ' + group + ' ' + name + ' ' + value);
}
function errName(e) {
  return (e && e.name ? e.name : 'Error') + ': ' + (e && e.message ? e.message : e);
}
function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}
