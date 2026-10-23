//! Function.prototype.

use crate::realm::{Realm, Res};
use crate::value::Val;

/// Function.prototype[Symbol.hasInstance].
pub(crate) fn has_instance(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let v = args.first().copied().unwrap_or(Val::Undef);
    Ok(Val::Bool(r.ordinary_has_instance(this, v)?))
}
