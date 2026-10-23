// crossorigin=use-credentials: imports carry the credentials too.
import { cookie as other } from 'http://api.milk.test:@PORT@/cors/cred.js?use';
report('cred', 'use-credentials', 'api: ' + other);
