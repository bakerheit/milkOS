//! ES modules: module records, linking and evaluation (see `api.rs` for the interface).
//! First commit: the table and a stub that refuses to compile anything.

use alloc::string::String;
use alloc::vec::Vec;

use crate::api::{
    DynamicImportId, ModuleId, ModuleOptions, ModuleRequest, ModuleStatus,
};
use crate::realm::{Realm, Res};
use crate::value::Val;

pub(crate) struct Modules {
    pub max: usize,
    live: usize,
}

impl Modules {
    pub fn new() -> Modules {
        Modules {
            max: 10_000,
            live: 0,
        }
    }
    pub fn count(&self) -> usize {
        self.live
    }
}

impl Realm {
    pub(crate) fn module_compile(&mut self, _source: &str, opts: &ModuleOptions<'_>) -> Res<ModuleId> {
        Err(self.syntax_error(&alloc::format!(
            "ES modules are not supported yet ({})",
            opts.file
        )))
    }
    pub(crate) fn module_synthetic(&mut self, _file: &str, _exports: Vec<(String, Val)>) -> Res<ModuleId> {
        Err(self.syntax_error("ES modules are not supported yet"))
    }
    pub(crate) fn module_requests_of(&self, _m: ModuleId) -> Vec<ModuleRequest> {
        Vec::new()
    }
    pub(crate) fn module_set_dependency(&mut self, _m: ModuleId, _i: usize, _d: ModuleId) -> Res<()> {
        Err(self.type_error("invalid module"))
    }
    pub(crate) fn module_dependency_of(&self, _m: ModuleId, _i: usize) -> Option<ModuleId> {
        None
    }
    pub(crate) fn module_link(&mut self, _m: ModuleId) -> Res<()> {
        Err(self.type_error("invalid module"))
    }
    pub(crate) fn module_evaluate(&mut self, _m: ModuleId) -> Res<Val> {
        Err(self.type_error("invalid module"))
    }
    pub(crate) fn module_ns(&mut self, _m: ModuleId) -> Res<Val> {
        Err(self.type_error("invalid module"))
    }
    pub(crate) fn module_status_of(&self, _m: ModuleId) -> Option<ModuleStatus> {
        None
    }
    pub(crate) fn module_error_of(&self, _m: ModuleId) -> Option<Val> {
        None
    }
    pub(crate) fn module_file_of(&self, _m: ModuleId) -> Option<String> {
        None
    }
    pub(crate) fn module_export_names_of(&mut self, _m: ModuleId) -> Res<Vec<String>> {
        Err(self.type_error("invalid module"))
    }
    pub(crate) fn dynamic_import_finish(
        &mut self,
        _id: DynamicImportId,
        _r: Result<ModuleId, Val>,
    ) -> Res<()> {
        Ok(())
    }
    pub(crate) fn dynamic_import_count(&self) -> usize {
        0
    }
    pub(crate) fn dynamic_import_cancel(&mut self) {}
    pub(crate) fn module_drop(&mut self, _m: ModuleId) {}
    pub(crate) fn module_drop_graph(&mut self, _m: ModuleId) {}
}
