// Same origin: the cookie goes (credentials mode same-origin); the other origin's does not.
import { cookie } from '/cookie/who.js';
import { cookie as other } from 'http://api.milk.test:@PORT@/cors/cred.js?same';
report('cred', 'same-origin-module', cookie + ' | api: ' + other);
