import { greet } from './lib/greet.mjs';
import version from './lib/version.mjs';
onmessage = async function (e) {
  const late = await import('./lib/late.mjs');
  postMessage(greet(e.data) + ' v' + version + ' late=' + late.value + ' url=' + new URL(import.meta.url).pathname);
};
