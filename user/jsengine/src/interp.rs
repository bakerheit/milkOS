//! The VM. (Stub: script functions arrive with the compiler.)

use alloc::string::String;
use alloc::vec::Vec;

use crate::heap::Tracer;
use crate::realm::{Realm, Res};
use crate::value::{ObjId, Val};

pub(crate) struct Frame;

pub(crate) struct VmState {
    pub frames: Vec<Frame>,
    pub stack: Vec<Val>,
}

impl VmState {
    pub fn new() -> VmState {
        VmState {
            frames: Vec::new(),
            stack: Vec::new(),
        }
    }
    pub fn trace(&self, t: &mut Tracer) {
        for v in &self.stack {
            t.val(*v);
        }
    }
}

impl Realm {
    pub(crate) fn vm_callable(&self, _o: ObjId) -> bool {
        false
    }
    pub(crate) fn vm_constructible(&self, _o: ObjId) -> bool {
        false
    }
    pub(crate) fn is_class_ctor(&self, o: ObjId) -> bool {
        self.o(o).flags & crate::object::CLASS_CTOR != 0
    }
    pub(crate) fn call_closure(
        &mut self,
        f: ObjId,
        _this: Val,
        _args: &[Val],
        _nt: Val,
    ) -> Res<Val> {
        Err(self.not_callable(Val::Obj(f)))
    }
    /// (function name, file, line, column), innermost first.
    pub(crate) fn stack_frames(&mut self, _max: usize) -> Vec<(String, String, u32, u32)> {
        Vec::new()
    }
    pub(crate) fn inspect_special(&mut self, _o: ObjId, _depth: u32, _top: bool) -> Option<String> {
        None
    }
}
