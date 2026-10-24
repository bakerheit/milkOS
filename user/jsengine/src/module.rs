//! ES modules: module records, linking and evaluation (see `api.rs` for the interface).
//!
//! A module is compiled to two codes that run in the module's own environment (one slot per
//! top-level binding): `init` creates the hoisted functions at link time, `body` is the
//! module code (an async function if it uses top-level `await`). Imported bindings are slots
//! that hold an `ImportRef` to the exporting module's slot, so they are live; `import * as`
//! bindings hold the namespace object. Linking and evaluation follow the spec's cyclic
//! module record algorithms (InnerModuleLinking, InnerModuleEvaluation, ExecuteAsyncModule,
//! AsyncModuleExecutionFulfilled / Rejected, GatherAvailableAncestors). The engine does no
//! I/O: the host answers each module request with a module.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use crate::api::{
    DynamicImport, DynamicImportId, Exc, Exception, ModuleId, ModuleOptions, ModuleRequest,
    ModuleStatus,
};
use crate::ast::ImportName;
use crate::bytecode::{Code, Source};
use crate::compiler::Compiler;
use crate::heap::Tracer;
use crate::interp::{Closure, EnvData};
use crate::names::N;
use crate::object::{NsData, NsTarget, ObjKind, Prop, PropKey, PropMap};
use crate::parser::Parser;
use crate::realm::{Realm, Res};
use crate::resolve::Resolver;
use crate::value::{ObjId, Val};

/// The most `import()` calls that may wait for the host at once; more reject.
const MAX_PENDING_IMPORTS: usize = 4096;

/// Where a module is in its life (the spec's cyclic module record status).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum St {
    Unlinked,
    Linking,
    Linked,
    Evaluating,
    EvaluatingAsync,
    Evaluated,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Binding {
    Slot(u32),
    Namespace,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Resolution {
    Found { module: usize, binding: Binding },
    NotFound,
    Ambiguous,
}

pub(crate) struct ReqRec {
    pub specifier: String,
    pub attrs: Vec<(String, String)>,
    pub line: u32,
    pub col: u32,
}

pub(crate) struct ImportRes {
    pub request: u32,
    pub name: ImportName,
    pub slot: u32,
    pub pos: u32,
}

pub(crate) struct IndirectRec {
    pub name: String,
    pub request: u32,
    pub import: ImportName,
    pub pos: u32,
}

pub(crate) struct ModuleRec {
    pub file: String,
    pub source: Option<Rc<Source>>,
    pub requests: Vec<ReqRec>,
    pub imports: Vec<ImportRes>,
    pub local_exports: Vec<(String, u32)>,
    pub indirect_exports: Vec<IndirectRec>,
    /// Export name -> position in `local_exports` / `indirect_exports` (modules can have
    /// thousands of exports).
    pub local_index: BTreeMap<String, usize>,
    pub indirect_index: BTreeMap<String, usize>,
    pub star_exports: Vec<(u32, u32)>,
    pub init: Option<Rc<Code>>,
    pub body: Option<Rc<Code>>,
    pub has_tla: bool,
    /// Slots of let/const/class bindings (uninitialized until the code runs).
    pub tdz: Vec<u32>,
    pub env: ObjId,
    pub ns: Option<ObjId>,
    pub meta: Option<ObjId>,
    pub deps: Vec<Option<ModuleId>>,
    pub status: St,
    pub eval_error: Option<Val>,
    pub dfs_index: u32,
    pub dfs_anc: u32,
    pub cycle_root: Option<usize>,
    /// The spec's [[AsyncEvaluation]] boolean, and the order it was set in.
    pub async_eval: bool,
    pub async_seq: u64,
    pub top_promise: Option<ObjId>,
    pub pending_deps: u32,
    pub async_parents: Vec<usize>,
    pub ext_bytes: usize,
    pub synthetic: bool,
}

impl ModuleRec {
    fn blank(env: ObjId) -> ModuleRec {
        ModuleRec {
            file: String::new(),
            source: None,
            requests: Vec::new(),
            imports: Vec::new(),
            local_exports: Vec::new(),
            indirect_exports: Vec::new(),
            local_index: BTreeMap::new(),
            indirect_index: BTreeMap::new(),
            star_exports: Vec::new(),
            init: None,
            body: None,
            has_tla: false,
            tdz: Vec::new(),
            env,
            ns: None,
            meta: None,
            deps: Vec::new(),
            status: St::Evaluated,
            eval_error: None,
            dfs_index: 0,
            dfs_anc: 0,
            cycle_root: None,
            async_eval: false,
            async_seq: 0,
            top_promise: None,
            pending_deps: 0,
            async_parents: Vec::new(),
            ext_bytes: 0,
            synthetic: false,
        }
    }
}

struct Slot {
    generation: u32,
    rec: Option<ModuleRec>,
}

pub(crate) struct Modules {
    pub max: usize,
    slots: Vec<Slot>,
    free: Vec<u32>,
    live: usize,
    async_counter: u64,
    /// `import()` calls waiting for the host: the Promise handed to script.
    pending: Vec<Option<ObjId>>,
    pending_free: Vec<u32>,
    pending_live: usize,
    /// What lookups of a missing module see.
    dummy: ModuleRec,
}

impl Modules {
    pub fn new() -> Modules {
        Modules {
            max: 10_000,
            slots: Vec::new(),
            free: Vec::new(),
            live: 0,
            async_counter: 0,
            pending: Vec::new(),
            pending_free: Vec::new(),
            pending_live: 0,
            dummy: ModuleRec::blank(ObjId { idx: 0, g: 0 }),
        }
    }

    pub fn count(&self) -> usize {
        self.live
    }

    fn reserve(&mut self) -> ModuleId {
        self.live += 1;
        if let Some(i) = self.free.pop() {
            ModuleId(i, self.slots[i as usize].generation)
        } else {
            self.slots.push(Slot {
                generation: 1,
                rec: None,
            });
            ModuleId(self.slots.len() as u32 - 1, 1)
        }
    }

    fn valid(&self, id: ModuleId) -> bool {
        self.slots
            .get(id.0 as usize)
            .is_some_and(|s| s.generation == id.1 && s.rec.is_some())
    }

    fn id_of(&self, idx: usize) -> ModuleId {
        ModuleId(idx as u32, self.slots[idx].generation)
    }

    pub fn trace(&self, t: &mut Tracer, epoch: u32) {
        for s in &self.slots {
            let Some(m) = &s.rec else { continue };
            t.obj(m.env);
            t.opt_obj(m.ns);
            t.opt_obj(m.meta);
            t.opt_obj(m.top_promise);
            if let Some(v) = m.eval_error {
                t.val(v);
            }
            if let Some(c) = &m.init {
                c.trace(t, epoch);
            }
            if let Some(c) = &m.body {
                c.trace(t, epoch);
            }
        }
        for p in self.pending.iter().flatten() {
            t.obj(*p);
        }
    }
}

fn st_api(s: St) -> ModuleStatus {
    match s {
        St::Unlinked => ModuleStatus::Unlinked,
        St::Linking => ModuleStatus::Linking,
        St::Linked => ModuleStatus::Linked,
        St::Evaluating => ModuleStatus::Evaluating,
        St::EvaluatingAsync => ModuleStatus::EvaluatingAsync,
        St::Evaluated => ModuleStatus::Evaluated,
    }
}

fn pack(id: ModuleId) -> Val {
    Val::Num(id.1 as f64 * 4_294_967_296.0 + id.0 as f64)
}

fn unpack(v: Val) -> Option<ModuleId> {
    let n = v.as_f64()?;
    Some(ModuleId(
        (n % 4_294_967_296.0) as u32,
        (n / 4_294_967_296.0) as u32,
    ))
}

fn code_bytes(c: &Code) -> usize {
    let mut n = 256
        + c.ops.len() * core::mem::size_of::<crate::bytecode::Op>()
        + c.lines.len() * 8
        + c.handlers.len() * 20
        + c.keys.len() * 16;
    for k in &c.consts {
        n += 24;
        if let crate::bytecode::Const::Code(sub) = k {
            n += code_bytes(sub);
        }
    }
    n
}

impl Realm {
    // ---- the table ----

    fn mrec(&self, i: usize) -> &ModuleRec {
        match self.modules.slots.get(i).and_then(|s| s.rec.as_ref()) {
            Some(r) => r,
            None => &self.modules.dummy,
        }
    }

    fn mrec_mut(&mut self, i: usize) -> &mut ModuleRec {
        let present = self.modules.slots.get(i).is_some_and(|s| s.rec.is_some());
        if present && let Some(r) = self.modules.slots[i].rec.as_mut() {
            return r;
        }
        &mut self.modules.dummy
    }

    /// A checked module id from the embedder: the table index.
    fn mid(&mut self, id: ModuleId) -> Res<usize> {
        if self.modules.valid(id) {
            Ok(id.0 as usize)
        } else {
            Err(self.type_error("invalid or dropped module"))
        }
    }

    /// The module that answers request `r` of module `m`, if the host set one that is still
    /// there.
    fn dep_idx(&self, m: usize, r: usize) -> Option<usize> {
        let id = (*self.mrec(m).deps.get(r)?)?;
        if self.modules.valid(id) {
            Some(id.0 as usize)
        } else {
            None
        }
    }

    /// Counts `n` units of work against the step budget and the interrupt.
    fn tick(&mut self, n: u64) -> Res<()> {
        self.steps += n;
        self.poll_steps += n;
        if self.poll_steps >= self.next_poll
            || (self.limits.max_steps != 0 && self.steps > self.limits.max_steps)
        {
            self.poll()?;
        }
        Ok(())
    }

    fn module_env_obj(&self, i: usize) -> ObjId {
        self.mrec(i).env
    }

    // ---- compiling ----

    pub(crate) fn module_compile(
        &mut self,
        source: &str,
        opts: &ModuleOptions<'_>,
    ) -> Res<ModuleId> {
        if source.len() > self.limits.max_source_bytes {
            return Err(self.range_error("The module is too large"));
        }
        if self.modules.live >= self.modules.max {
            return Err(self.range_error("Too many modules"));
        }
        let text: Rc<[u8]> = Rc::from(source.as_bytes());
        let src = Source {
            text: text.clone(),
            file: String::from(opts.file),
            line_starts: crate::lexer::line_starts(&text),
            line_delta: opts.first_line as i32 - 1,
            first_col: opts.first_column.max(1),
        };
        self.budget_check(source.len() / 2)?;
        let mut p = Parser::new(&text);
        p.stack = Some(self.stack);
        p.mem_limit = self.compile_limit();
        p.lazy_min = crate::lazy::lazy_min_bytes();
        let ast = match p.parse_module() {
            Ok(a) => a,
            Err(e) => return Err(self.syntax_error_at(&src, e.pos, &e.msg)),
        };
        let atoms = core::mem::replace(&mut p.atoms, crate::atoms::Atoms::empty());
        let nscopes = p.scope_count();
        drop(p);
        let mut res = Resolver::new(&atoms, nscopes);
        res.stack = Some(self.stack);
        if let Err(e) = res.module(&ast) {
            return Err(self.syntax_error_at(&src, e.pos, &e.msg));
        }
        let scopes = core::mem::take(&mut res.scopes);
        // The static description of the module, from the finished scope table.
        let sc = &scopes[ast.scope];
        let slot_of = |a: u32| sc.find(a).map(|i| i as u32).unwrap_or(0);
        let tdz: Vec<u32> = sc
            .bindings
            .iter()
            .enumerate()
            .filter(|(_, b)| b.kind.is_lexical())
            .map(|(i, _)| i as u32)
            .collect();
        let info = &ast.info;
        let requests: Vec<ReqRec> = info
            .requests
            .iter()
            .map(|r| {
                let (line, col) = src.line_col(r.pos);
                ReqRec {
                    specifier: r.specifier.clone(),
                    attrs: r.attrs.clone(),
                    line,
                    col,
                }
            })
            .collect();
        let local_exports: Vec<(String, u32)> = info
            .local_exports
            .iter()
            .map(|e| (e.export_name.clone(), slot_of(e.local)))
            .collect();
        let star_exports: Vec<(u32, u32)> =
            info.star_exports.iter().map(|(r, p)| (*r, *p)).collect();
        let has_tla = info.has_tla;
        let imports: Vec<ImportRes> = info
            .imports
            .iter()
            .map(|i| ImportRes {
                request: i.request,
                name: i.name.clone_name(),
                slot: slot_of(i.local),
                pos: i.pos,
            })
            .collect();
        let indirect_exports: Vec<IndirectRec> = info
            .indirect_exports
            .iter()
            .map(|e| IndirectRec {
                name: e.export_name.clone(),
                request: e.request,
                import: e.name.clone_name(),
                pos: e.pos,
            })
            .collect();
        let id = self.modules.reserve();
        let src = Rc::new(src);
        let shared = Rc::new(crate::lazy::LazyShared {
            atoms: core::cell::RefCell::new(None),
            default_atom: Some(ast.info.default_atom),
        });
        let compiled = {
            let mut c = Compiler::new(self, &atoms, scopes, src.clone());
            c.set_lazy_shared(shared.clone());
            match c.compile_module(&ast, id) {
                Ok(r) => Ok(r),
                Err(e) => Err((e.pos, e.msg)),
            }
        };
        // Functions left for their first call keep the identifier table.
        drop(ast);
        if Rc::strong_count(&shared) > 1 {
            *shared.atoms.borrow_mut() = Some(atoms);
        }
        let (init, body, n) = match compiled {
            Ok(r) => r,
            Err((pos, msg)) => {
                self.module_release_slot(id);
                return Err(self.syntax_error_at(&src, pos, &msg));
            }
        };
        let mut slots = alloc::vec![Val::Undef; n as usize];
        for &t in &tdz {
            slots[t as usize] = Val::Empty;
        }
        let env = self.alloc(
            None,
            ObjKind::Env(Box::new(EnvData {
                slots,
                parent: None,
            })),
        );
        let mut rec = ModuleRec::blank(env);
        rec.file = String::from(opts.file);
        rec.deps = alloc::vec![None; requests.len()];
        rec.requests = requests;
        rec.imports = imports;
        rec.local_index = local_exports
            .iter()
            .enumerate()
            .map(|(i, (n, _))| (n.clone(), i))
            .collect();
        rec.indirect_index = indirect_exports
            .iter()
            .enumerate()
            .map(|(i, e)| (e.name.clone(), i))
            .collect();
        rec.local_exports = local_exports;
        rec.indirect_exports = indirect_exports;
        rec.star_exports = star_exports;
        rec.has_tla = has_tla;
        rec.tdz = tdz;
        rec.status = St::Unlinked;
        let ext = code_bytes(&init) + code_bytes(&body) + source.len() / 2 + 512;
        rec.ext_bytes = ext;
        rec.init = Some(init);
        rec.body = Some(body);
        rec.source = Some(src);
        self.heap.external += ext;
        self.heap.charge(ext);
        self.modules.slots[id.0 as usize].rec = Some(rec);
        if let Err(e) = self.budget_check(0) {
            self.module_drop(id);
            return Err(e);
        }
        Ok(id)
    }

    /// Fails with an out-of-memory termination if `extra` more bytes can't fit the heap budget
    /// even after a collection (when one is possible here).
    fn budget_check(&mut self, extra: usize) -> Res<()> {
        let max = self.heap.max_bytes;
        if self.heap.used().saturating_add(extra) <= max {
            return Ok(());
        }
        if self.native_frames.is_empty() && self.frames_empty() {
            self.collect_garbage();
        }
        if self.heap.live_bytes.saturating_add(extra) > max {
            return Err(self.oom());
        }
        Ok(())
    }

    fn module_release_slot(&mut self, id: ModuleId) {
        if let Some(s) = self.modules.slots.get_mut(id.0 as usize)
            && s.generation == id.1
        {
            s.rec = None;
            s.generation = s.generation.wrapping_add(1).max(1);
            self.modules.free.push(id.0);
            self.modules.live = self.modules.live.saturating_sub(1);
        }
    }

    pub(crate) fn module_synthetic(
        &mut self,
        file: &str,
        exports: Vec<(String, Val)>,
    ) -> Res<ModuleId> {
        if self.modules.live >= self.modules.max {
            return Err(self.range_error("Too many modules"));
        }
        let mut names: BTreeSet<&str> = BTreeSet::new();
        for (n, _) in &exports {
            if !names.insert(n.as_str()) {
                return Err(self.type_error("duplicate export name in a synthetic module"));
            }
        }
        let slots: Vec<Val> = exports.iter().map(|(_, v)| *v).collect();
        let env = self.alloc(
            None,
            ObjKind::Env(Box::new(EnvData {
                slots,
                parent: None,
            })),
        );
        let mut rec = ModuleRec::blank(env);
        rec.file = String::from(file);
        rec.local_exports = exports
            .iter()
            .enumerate()
            .map(|(i, (n, _))| (n.clone(), i as u32))
            .collect();
        rec.local_index = exports
            .iter()
            .enumerate()
            .map(|(i, (n, _))| (n.clone(), i))
            .collect();
        rec.synthetic = true;
        rec.status = St::Evaluated;
        rec.ext_bytes = 256 + exports.len() * 64;
        self.heap.external += rec.ext_bytes;
        let id = self.modules.reserve();
        rec.cycle_root = Some(id.0 as usize);
        self.modules.slots[id.0 as usize].rec = Some(rec);
        Ok(id)
    }

    // ---- simple accessors for the API ----

    pub(crate) fn module_requests_of(&self, m: ModuleId) -> Vec<ModuleRequest> {
        if !self.modules.valid(m) {
            return Vec::new();
        }
        self.mrec(m.0 as usize)
            .requests
            .iter()
            .map(|r| ModuleRequest {
                specifier: r.specifier.clone(),
                attributes: r.attrs.clone(),
                line: r.line,
                column: r.col,
            })
            .collect()
    }

    pub(crate) fn module_set_dependency(
        &mut self,
        m: ModuleId,
        index: usize,
        dep: ModuleId,
    ) -> Res<()> {
        let mi = self.mid(m)?;
        self.mid(dep)?;
        let st = self.mrec(mi).status;
        if !matches!(st, St::Unlinked) {
            return Err(self.type_error("the module is already linked"));
        }
        if index >= self.mrec(mi).requests.len() {
            return Err(self.range_error("no such module request"));
        }
        self.mrec_mut(mi).deps[index] = Some(dep);
        Ok(())
    }

    pub(crate) fn module_dependency_of(&self, m: ModuleId, index: usize) -> Option<ModuleId> {
        if !self.modules.valid(m) {
            return None;
        }
        let d = (*self.mrec(m.0 as usize).deps.get(index)?)?;
        if self.modules.valid(d) { Some(d) } else { None }
    }

    pub(crate) fn module_status_of(&self, m: ModuleId) -> Option<ModuleStatus> {
        if !self.modules.valid(m) {
            return None;
        }
        Some(st_api(self.mrec(m.0 as usize).status))
    }

    pub(crate) fn module_error_of(&self, m: ModuleId) -> Option<Val> {
        if !self.modules.valid(m) {
            return None;
        }
        self.mrec(m.0 as usize).eval_error
    }

    pub(crate) fn module_file_of(&self, m: ModuleId) -> Option<String> {
        if !self.modules.valid(m) {
            return None;
        }
        Some(self.mrec(m.0 as usize).file.clone())
    }

    pub(crate) fn module_export_names_of(&mut self, m: ModuleId) -> Res<Vec<String>> {
        let mi = self.mid(m)?;
        if matches!(self.mrec(mi).status, St::Unlinked) {
            return Err(self.type_error("the module is not linked"));
        }
        let names = self.sorted_exports(mi)?;
        Ok(names.into_iter().map(|(n, _, _)| n).collect())
    }

    pub(crate) fn module_drop(&mut self, m: ModuleId) {
        if !self.modules.valid(m) {
            return;
        }
        let ext = self.mrec(m.0 as usize).ext_bytes;
        self.heap.external = self.heap.external.saturating_sub(ext);
        self.module_release_slot(m);
    }

    pub(crate) fn module_drop_graph(&mut self, m: ModuleId) {
        let mut todo = alloc::vec![m];
        let mut seen: BTreeSet<u32> = BTreeSet::new();
        while let Some(x) = todo.pop() {
            if !self.modules.valid(x) || !seen.insert(x.0) {
                continue;
            }
            for d in self.mrec(x.0 as usize).deps.iter().flatten() {
                todo.push(*d);
            }
            self.module_drop(x);
        }
    }

    // ---- export resolution ----

    fn star_deps(&self, m: usize) -> Vec<usize> {
        self.mrec(m)
            .star_exports
            .iter()
            .filter_map(|(r, _)| self.dep_idx(m, *r as usize))
            .collect()
    }

    /// GetExportedNames.
    fn exported_names(&mut self, m: usize, visited: &mut BTreeSet<usize>) -> Res<Vec<String>> {
        self.check_stack()?;
        self.tick(1)?;
        if !visited.insert(m) {
            return Ok(Vec::new());
        }
        let mut names: Vec<String> = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for (n, _) in &self.mrec(m).local_exports {
            if seen.insert(n.clone()) {
                names.push(n.clone());
            }
        }
        for e in &self.mrec(m).indirect_exports {
            if seen.insert(e.name.clone()) {
                names.push(e.name.clone());
            }
        }
        for dep in self.star_deps(m) {
            for n in self.exported_names(dep, visited)? {
                if n != "default" && seen.insert(n.clone()) {
                    names.push(n);
                }
            }
        }
        Ok(names)
    }

    /// ResolveExport.
    fn resolve_export(
        &mut self,
        m: usize,
        name: &str,
        set: &mut Vec<(usize, String)>,
    ) -> Res<Resolution> {
        self.check_stack()?;
        self.tick(1)?;
        if set.iter().any(|(sm, sn)| *sm == m && sn == name) {
            return Ok(Resolution::NotFound);
        }
        set.push((m, String::from(name)));
        if let Some(&i) = self.mrec(m).local_index.get(name) {
            let slot = self.mrec(m).local_exports[i].1;
            return Ok(Resolution::Found {
                module: m,
                binding: Binding::Slot(slot),
            });
        }
        let ind = self.mrec(m).indirect_index.get(name).map(|&i| {
            let e = &self.mrec(m).indirect_exports[i];
            (e.request, e.import.clone_name())
        });
        if let Some((req, imp)) = ind {
            let Some(dep) = self.dep_idx(m, req as usize) else {
                return Ok(Resolution::NotFound);
            };
            return match imp {
                ImportName::All => Ok(Resolution::Found {
                    module: dep,
                    binding: Binding::Namespace,
                }),
                ImportName::Name(n) => self.resolve_export(dep, &n, set),
            };
        }
        if name == "default" {
            return Ok(Resolution::NotFound);
        }
        let mut star: Option<Resolution> = None;
        for dep in self.star_deps(m) {
            let r = self.resolve_export(dep, name, set)?;
            match r {
                Resolution::Ambiguous => return Ok(Resolution::Ambiguous),
                Resolution::NotFound => {}
                found => match &star {
                    None => star = Some(found),
                    Some(prev) => {
                        if *prev != found {
                            return Ok(Resolution::Ambiguous);
                        }
                    }
                },
            }
        }
        Ok(star.unwrap_or(Resolution::NotFound))
    }

    /// The unambiguously resolvable exports of `m`, sorted by code units.
    fn sorted_exports(&mut self, m: usize) -> Res<Vec<(String, usize, Binding)>> {
        let names = self.exported_names(m, &mut BTreeSet::new())?;
        let mut out: Vec<(String, usize, Binding)> = Vec::new();
        for n in names {
            if let Resolution::Found { module, binding } =
                self.resolve_export(m, &n, &mut Vec::new())?
            {
                out.push((n, module, binding));
            }
        }
        out.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
        Ok(out)
    }

    // ---- namespaces ----

    pub(crate) fn module_ns(&mut self, id: ModuleId) -> Res<Val> {
        let mi = self.mid(id)?;
        if matches!(self.mrec(mi).status, St::Unlinked) {
            return Err(self.type_error("the module is not linked"));
        }
        Ok(Val::Obj(self.get_module_namespace(mi)?))
    }

    /// GetModuleNamespace.
    fn get_module_namespace(&mut self, m: usize) -> Res<ObjId> {
        if let Some(ns) = self.mrec(m).ns {
            return Ok(ns);
        }
        self.check_stack()?;
        // The object exists (empty) before its entries are worked out: a namespace that
        // re-exports itself finds it.
        let ns = self.alloc(
            None,
            ObjKind::Namespace(Box::new(NsData {
                map: PropMap::new(),
                order: Vec::new(),
                targets: Vec::new(),
            })),
        );
        self.om(ns).flags &= !crate::object::EXTENSIBLE;
        let tag_key = PropKey::Sym(self.sym.to_string_tag);
        let tag = self.str_val("Module");
        self.om(ns).props.insert(tag_key, Prop { v: tag, f: 0 });
        self.mrec_mut(m).ns = Some(ns);
        let r = self.fill_namespace(m, ns);
        if r.is_err() {
            self.mrec_mut(m).ns = None;
        }
        r?;
        Ok(ns)
    }

    fn fill_namespace(&mut self, m: usize, ns: ObjId) -> Res<()> {
        let entries = self.sorted_exports(m)?;
        let mut map = PropMap::new();
        let mut order = Vec::with_capacity(entries.len());
        let mut targets = Vec::with_capacity(entries.len());
        for (i, (name, module, binding)) in entries.into_iter().enumerate() {
            let key = self.key_str(&name);
            map.insert(
                key,
                Prop {
                    v: Val::Int(i as i32),
                    f: 0,
                },
            );
            order.push(key);
            targets.push(match binding {
                Binding::Slot(s) => NsTarget::Slot(self.module_env_obj(module), s),
                Binding::Namespace => NsTarget::Value(Val::Obj(self.get_module_namespace(module)?)),
            });
        }
        self.heap.charge(order.len() * 72);
        if let ObjKind::Namespace(d) = &mut self.om(ns).kind {
            d.map = map;
            d.order = order;
            d.targets = targets;
        }
        Ok(())
    }

    /// The current value of a namespace entry (Empty before its binding is initialized).
    pub(crate) fn ns_value(&self, t: &NsTarget) -> Val {
        match t {
            NsTarget::Value(v) => *v,
            NsTarget::Slot(env, slot) => match &self.o(*env).kind {
                ObjKind::Env(d) => d.slots.get(*slot as usize).copied().unwrap_or(Val::Empty),
                _ => Val::Empty,
            },
        }
    }

    /// `ns[k]` for a string key: None when `k` is not an export, a ReferenceError while its
    /// binding is uninitialized.
    pub(crate) fn ns_get(&mut self, id: ObjId, k: PropKey) -> Res<Option<Val>> {
        let v = match &self.o(id).kind {
            ObjKind::Namespace(ns) => match ns.map.get(k) {
                Some(Prop { v: Val::Int(i), .. }) => self.ns_value(&ns.targets[*i as usize]),
                _ => return Ok(None),
            },
            _ => return Ok(None),
        };
        if v.is_empty() {
            let name = self.key_display(k);
            return Err(self.reference_error(&alloc::format!(
                "Cannot access '{name}' before initialization"
            )));
        }
        Ok(Some(v))
    }

    // ---- linking ----

    pub(crate) fn module_link(&mut self, id: ModuleId) -> Res<()> {
        let mi = self.mid(id)?;
        self.link_idx(mi)
    }

    fn link_idx(&mut self, m: usize) -> Res<()> {
        let mut stack: Vec<usize> = Vec::new();
        match self.inner_link(m, &mut stack, 0) {
            Ok(_) => Ok(()),
            Err(e) => {
                for s in stack {
                    let r = self.mrec_mut(s);
                    r.status = St::Unlinked;
                    r.dfs_index = 0;
                    r.dfs_anc = 0;
                }
                Err(e)
            }
        }
    }

    fn inner_link(&mut self, m: usize, stack: &mut Vec<usize>, mut index: u32) -> Res<u32> {
        self.check_stack()?;
        self.tick(1)?;
        if self.mrec(m).status != St::Unlinked {
            return Ok(index);
        }
        {
            let r = self.mrec_mut(m);
            r.status = St::Linking;
            r.dfs_index = index;
            r.dfs_anc = index;
        }
        index += 1;
        stack.push(m);
        let nreq = self.mrec(m).requests.len();
        for r in 0..nreq {
            let Some(dep) = self.dep_idx(m, r) else {
                let (spec, line, col) = {
                    let q = &self.mrec(m).requests[r];
                    (q.specifier.clone(), q.line, q.col)
                };
                let file = self.mrec(m).file.clone();
                return Err(self.type_error(&alloc::format!(
                    "Cannot link: the module request '{spec}' at {file}:{line}:{col} was not resolved"
                )));
            };
            index = self.inner_link(dep, stack, index)?;
            if self.mrec(dep).status == St::Linking {
                let da = self.mrec(dep).dfs_anc;
                let mr = self.mrec_mut(m);
                mr.dfs_anc = mr.dfs_anc.min(da);
            }
        }
        self.init_env(m)?;
        if self.mrec(m).dfs_anc == self.mrec(m).dfs_index {
            while let Some(x) = stack.pop() {
                self.mrec_mut(x).status = St::Linked;
                if x == m {
                    break;
                }
            }
        }
        Ok(index)
    }

    fn link_error(&mut self, m: usize, pos: u32, msg: &str) -> Exception {
        match self.mrec(m).source.clone() {
            Some(src) => self.syntax_error_at(&src, pos, msg),
            None => self.syntax_error(msg),
        }
    }

    /// InitializeEnvironment: checks the indirect exports, binds the imports, makes the
    /// hoisted functions.
    fn init_env(&mut self, m: usize) -> Res<()> {
        // Indirect exports must resolve.
        let n_ind = self.mrec(m).indirect_exports.len();
        for i in 0..n_ind {
            let (name, req, pos) = {
                let e = &self.mrec(m).indirect_exports[i];
                (e.name.clone(), e.request, e.pos)
            };
            let res = self.resolve_export(m, &name, &mut Vec::new())?;
            match res {
                Resolution::Found { .. } => {}
                other => {
                    let spec = self.mrec(m).requests[req as usize].specifier.clone();
                    let msg = self.resolution_message(&spec, &name, &other);
                    return Err(self.link_error(m, pos, &msg));
                }
            }
        }
        // Reset the environment (a failed earlier link may have left things in it).
        let env = self.mrec(m).env;
        let tdz = self.mrec(m).tdz.clone();
        if let ObjKind::Env(d) = &mut self.om(env).kind {
            for s in d.slots.iter_mut() {
                *s = Val::Undef;
            }
            for t in &tdz {
                if let Some(s) = d.slots.get_mut(*t as usize) {
                    *s = Val::Empty;
                }
            }
        }
        // Imports.
        let n_imp = self.mrec(m).imports.len();
        for i in 0..n_imp {
            self.tick(1)?;
            let (req, name, slot, pos) = {
                let e = &self.mrec(m).imports[i];
                (e.request, e.name.clone_name(), e.slot, e.pos)
            };
            let Some(dep) = self.dep_idx(m, req as usize) else {
                return Err(self.type_error("a module request was not resolved"));
            };
            let value = match name {
                ImportName::All => Val::Obj(self.get_module_namespace(dep)?),
                ImportName::Name(n) => {
                    let res = self.resolve_export(dep, &n, &mut Vec::new())?;
                    match res {
                        Resolution::Found { module, binding } => match binding {
                            Binding::Slot(s) => {
                                let e = self.module_env_obj(module);
                                Val::Obj(self.alloc(None, ObjKind::ImportRef { env: e, slot: s }))
                            }
                            Binding::Namespace => {
                                let nsobj = self.get_module_namespace(module)?;
                                let holder = self.alloc(
                                    None,
                                    ObjKind::Env(Box::new(EnvData {
                                        slots: alloc::vec![Val::Obj(nsobj)],
                                        parent: None,
                                    })),
                                );
                                Val::Obj(self.alloc(
                                    None,
                                    ObjKind::ImportRef {
                                        env: holder,
                                        slot: 0,
                                    },
                                ))
                            }
                        },
                        other => {
                            let spec = self.mrec(m).requests[req as usize].specifier.clone();
                            let msg = self.resolution_message(&spec, &n, &other);
                            return Err(self.link_error(m, pos, &msg));
                        }
                    }
                }
            };
            if let ObjKind::Env(d) = &mut self.om(env).kind
                && let Some(s) = d.slots.get_mut(slot as usize)
            {
                *s = value;
            }
        }
        // The hoisted functions.
        if let Some(init) = self.mrec(m).init.clone() {
            let f = self.module_closure(init, env);
            self.call_module_fn(f)?;
        }
        Ok(())
    }

    fn resolution_message(&self, spec: &str, name: &str, r: &Resolution) -> String {
        match r {
            Resolution::Ambiguous => alloc::format!(
                "The requested module '{spec}' contains conflicting star exports for name '{name}'"
            ),
            _ => alloc::format!(
                "The requested module '{spec}' does not provide an export named '{name}'"
            ),
        }
    }

    fn module_closure(&mut self, code: Rc<Code>, env: ObjId) -> ObjId {
        let cl = Closure {
            code,
            env: Some(env),
            this: Val::Undef,
            new_target: Val::Undef,
            home: None,
            func_ctx: None,
            fields: None,
            field_keys: Vec::new(),
        };
        let p = self.intr.function_proto;
        let id = self.alloc(Some(p), ObjKind::Closure(Box::new(cl)));
        self.om(id).flags |= crate::object::LAZY_FN;
        id
    }

    fn call_module_fn(&mut self, f: ObjId) -> Res<Val> {
        let mark = self.temps.len();
        self.temps.push(Val::Obj(f));
        let r = self.call_closure(f, Val::Undef, &[], Val::Undef);
        self.temps.truncate(mark);
        r
    }

    // ---- evaluation ----

    pub(crate) fn module_evaluate(&mut self, id: ModuleId) -> Res<Val> {
        let mi = self.mid(id)?;
        if self.mrec(mi).status == St::Unlinked {
            self.link_idx(mi)?;
        }
        self.evaluate_idx(mi)
    }

    fn settled_promise(&mut self, ok: bool, v: Val) -> Val {
        let p = self.new_promise_obj(None);
        if let ObjKind::Promise(d) = &mut self.om(p).kind {
            d.handled = true;
        }
        if ok {
            self.fulfill_promise(p, v);
        } else {
            self.reject_promise(p, v);
        }
        Val::Obj(p)
    }

    fn evaluate_idx(&mut self, m0: usize) -> Res<Val> {
        let mut m = m0;
        match self.mrec(m).status {
            St::Linking | St::Unlinked => {
                return Err(self.type_error("the module is not linked"));
            }
            St::EvaluatingAsync | St::Evaluated => {
                if let Some(cr) = self.mrec(m).cycle_root {
                    m = cr;
                }
            }
            _ => {}
        }
        if let Some(p) = self.mrec(m).top_promise {
            return Ok(Val::Obj(p));
        }
        if let Some(e) = self.mrec(m).eval_error {
            return Ok(self.settled_promise(false, e));
        }
        if self.mrec(m).status == St::Evaluating {
            return Err(self.type_error("the module is being evaluated"));
        }
        let cap = self.new_promise_obj(None);
        if let ObjKind::Promise(d) = &mut self.om(cap).kind {
            d.handled = true;
        }
        self.mrec_mut(m).top_promise = Some(cap);
        let mut stack: Vec<usize> = Vec::new();
        match self.inner_eval(m, &mut stack, 0) {
            Err(e) => {
                let err = match e.0 {
                    Exc::Throw(v) => v,
                    Exc::Term(_) => {
                        let er = self.make_error(
                            crate::api::ErrorKind::Error,
                            "Module evaluation was terminated",
                        );
                        Val::Obj(er)
                    }
                };
                for s in stack {
                    let r = self.mrec_mut(s);
                    r.status = St::Evaluated;
                    r.eval_error = Some(err);
                }
                let r = self.mrec_mut(m);
                r.status = St::Evaluated;
                r.eval_error = Some(err);
                if e.is_uncatchable() {
                    return Err(e);
                }
                self.reject_promise(cap, err);
            }
            Ok(_) => {
                if !self.mrec(m).async_eval {
                    self.fulfill_promise(cap, Val::Undef);
                }
            }
        }
        Ok(Val::Obj(cap))
    }

    fn inner_eval(&mut self, m: usize, stack: &mut Vec<usize>, mut index: u32) -> Res<u32> {
        self.check_stack()?;
        self.tick(1)?;
        match self.mrec(m).status {
            St::EvaluatingAsync | St::Evaluated => {
                return match self.mrec(m).eval_error {
                    None => Ok(index),
                    Some(e) => Err(Exception(Exc::Throw(e))),
                };
            }
            St::Evaluating => return Ok(index),
            St::Linked => {}
            _ => return Err(self.type_error("the module is not linked")),
        }
        {
            let r = self.mrec_mut(m);
            r.status = St::Evaluating;
            r.dfs_index = index;
            r.dfs_anc = index;
            r.pending_deps = 0;
        }
        index += 1;
        stack.push(m);
        let nreq = self.mrec(m).requests.len();
        for r in 0..nreq {
            let Some(mut dep) = self.dep_idx(m, r) else {
                return Err(self.type_error("a module request was not resolved"));
            };
            index = self.inner_eval(dep, stack, index)?;
            if self.mrec(dep).status == St::Evaluating {
                let da = self.mrec(dep).dfs_anc;
                let mr = self.mrec_mut(m);
                mr.dfs_anc = mr.dfs_anc.min(da);
            } else {
                if let Some(cr) = self.mrec(dep).cycle_root {
                    dep = cr;
                }
                if let Some(e) = self.mrec(dep).eval_error {
                    return Err(Exception(Exc::Throw(e)));
                }
            }
            if self.mrec(dep).async_eval {
                self.mrec_mut(m).pending_deps += 1;
                self.mrec_mut(dep).async_parents.push(m);
            }
        }
        if self.mrec(m).pending_deps > 0 || self.mrec(m).has_tla {
            self.modules.async_counter += 1;
            let seq = self.modules.async_counter;
            {
                let r = self.mrec_mut(m);
                r.async_eval = true;
                r.async_seq = seq;
            }
            if self.mrec(m).pending_deps == 0 {
                self.execute_async_module(m)?;
            }
        } else {
            self.execute_module(m)?;
        }
        if self.mrec(m).dfs_anc == self.mrec(m).dfs_index {
            while let Some(x) = stack.pop() {
                let a = self.mrec(x).async_eval;
                let r = self.mrec_mut(x);
                r.status = if a {
                    St::EvaluatingAsync
                } else {
                    St::Evaluated
                };
                r.cycle_root = Some(m);
                if x == m {
                    break;
                }
            }
        }
        Ok(index)
    }

    /// ExecuteModule without a capability: runs the body synchronously.
    fn execute_module(&mut self, m: usize) -> Res<()> {
        let Some(body) = self.mrec(m).body.clone() else {
            return Ok(());
        };
        let env = self.mrec(m).env;
        let f = self.module_closure(body, env);
        self.call_module_fn(f)?;
        Ok(())
    }

    /// ExecuteAsyncModule: runs the body as an async function and arranges for the
    /// fulfilled / rejected handlers.
    fn execute_async_module(&mut self, m: usize) -> Res<()> {
        let Some(body) = self.mrec(m).body.clone() else {
            return Ok(());
        };
        let env = self.mrec(m).env;
        let is_async = body.is_async;
        let f = self.module_closure(body, env);
        let mark = self.temps.len();
        let r = self.call_module_fn(f);
        let promise = match r {
            Ok(Val::Obj(p)) if is_async => p,
            Ok(_) => {
                let p = self.new_promise_obj(None);
                self.temps.push(Val::Obj(p));
                self.fulfill_promise(p, Val::Undef);
                p
            }
            Err(Exception(Exc::Throw(e))) => {
                let p = self.new_promise_obj(None);
                self.temps.push(Val::Obj(p));
                self.reject_promise(p, e);
                p
            }
            Err(e) => {
                self.temps.truncate(mark);
                return Err(e);
            }
        };
        self.temps.push(Val::Obj(promise));
        let id = self.modules.id_of(m);
        let e = Val::Str(self.nstr(N::empty));
        let ok = self.new_builtin_slots(e, 1, async_module_fulfilled, alloc::vec![pack(id)]);
        let bad = self.new_builtin_slots(e, 1, async_module_rejected, alloc::vec![pack(id)]);
        self.perform_then(
            promise,
            crate::builtins::promise::Handler::Js(Val::Obj(ok)),
            crate::builtins::promise::Handler::Js(Val::Obj(bad)),
            None,
        );
        self.temps.truncate(mark);
        Ok(())
    }

    fn gather_available_ancestors(&mut self, m: usize, exec: &mut Vec<usize>) {
        let parents = self.mrec(m).async_parents.clone();
        for p in parents {
            if exec.contains(&p) {
                continue;
            }
            let root_failed = self
                .mrec(self.mrec(p).cycle_root.unwrap_or(p))
                .eval_error
                .is_some();
            if root_failed {
                continue;
            }
            let (pending, tla) = {
                let r = self.mrec_mut(p);
                if r.pending_deps > 0 {
                    r.pending_deps -= 1;
                }
                (r.pending_deps, r.has_tla)
            };
            if pending == 0 {
                exec.push(p);
                if !tla {
                    self.gather_available_ancestors(p, exec);
                }
            }
        }
    }

    fn async_fulfilled(&mut self, m: usize) -> Res<()> {
        if self.mrec(m).status == St::Evaluated {
            return Ok(());
        }
        {
            let r = self.mrec_mut(m);
            r.async_eval = false;
            r.status = St::Evaluated;
        }
        if let Some(p) = self.mrec(m).top_promise {
            self.fulfill_promise(p, Val::Undef);
        }
        let mut exec: Vec<usize> = Vec::new();
        self.gather_available_ancestors(m, &mut exec);
        exec.sort_by_key(|i| self.mrec(*i).async_seq);
        for x in exec {
            if self.mrec(x).status == St::Evaluated {
                continue;
            }
            if self.mrec(x).has_tla {
                self.execute_async_module(x)?;
            } else {
                match self.execute_module(x) {
                    Ok(()) => {
                        {
                            let r = self.mrec_mut(x);
                            r.async_eval = false;
                            r.status = St::Evaluated;
                        }
                        if let Some(p) = self.mrec(x).top_promise {
                            self.fulfill_promise(p, Val::Undef);
                        }
                    }
                    Err(Exception(Exc::Throw(e))) => self.async_rejected(x, e)?,
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(())
    }

    fn async_rejected(&mut self, m: usize, err: Val) -> Res<()> {
        self.check_stack()?;
        if self.mrec(m).status == St::Evaluated {
            return Ok(());
        }
        {
            let r = self.mrec_mut(m);
            r.eval_error = Some(err);
            r.status = St::Evaluated;
            r.async_eval = false;
        }
        let parents = self.mrec(m).async_parents.clone();
        for p in parents {
            self.async_rejected(p, err)?;
        }
        if let Some(p) = self.mrec(m).top_promise {
            self.reject_promise(p, err);
        }
        Ok(())
    }

    // ---- import.meta ----

    pub(crate) fn import_meta(&mut self, module: Option<ModuleId>) -> Res<Val> {
        let Some(id) = module else {
            return Err(self.syntax_error("Cannot use 'import.meta' outside a module"));
        };
        if !self.modules.valid(id) {
            // A dropped module whose functions still run: a fresh, empty object.
            return Ok(Val::Obj(self.alloc(None, ObjKind::Ordinary)));
        }
        let mi = id.0 as usize;
        if let Some(o) = self.mrec(mi).meta {
            return Ok(Val::Obj(o));
        }
        let o = self.alloc(None, ObjKind::Ordinary);
        self.mrec_mut(mi).meta = Some(o);
        if let Some(mut hook) = self.import_meta_hook.take() {
            let mark = self.temps.len();
            self.temps.push(Val::Obj(o));
            let r = hook(self, id, crate::api::Value(Val::Obj(o)));
            self.temps.truncate(mark);
            if self.import_meta_hook.is_none() {
                self.import_meta_hook = Some(hook);
            }
            r?;
        }
        Ok(Val::Obj(o))
    }

    // ---- import() ----

    pub(crate) fn import_call(
        &mut self,
        referrer: Option<ModuleId>,
        file: String,
        spec: Val,
        opts: Val,
    ) -> Res<Val> {
        // (no resolving functions: the engine settles it directly)
        let promise = self.new_promise_obj(None);
        let mark = self.temps.len();
        self.temps.push(Val::Obj(promise));
        let r = self.import_call_inner(promise, referrer, file, spec, opts);
        self.temps.truncate(mark);
        match r {
            Ok(()) => Ok(Val::Obj(promise)),
            Err(Exception(Exc::Throw(e))) => {
                self.reject_promise(promise, e);
                Ok(Val::Obj(promise))
            }
            Err(e) => Err(e),
        }
    }

    fn import_call_inner(
        &mut self,
        promise: ObjId,
        referrer: Option<ModuleId>,
        file: String,
        spec: Val,
        opts: Val,
    ) -> Res<()> {
        let s = self.to_str(spec)?;
        let specifier = self.rust_string(s);
        let mut attributes: Vec<(String, String)> = Vec::new();
        if !opts.is_undef() {
            let Val::Obj(oo) = opts else {
                return Err(self.type_error("The second argument of import() must be an object"));
            };
            let wk = self.key_str("with");
            let w = self.get_p(oo, wk, opts)?;
            if !w.is_undef() {
                let Val::Obj(wo) = w else {
                    return Err(self.type_error("The 'with' option of import() must be an object"));
                };
                let keys = self.own_keys_p(wo, true, false, true)?;
                for k in keys {
                    let v = self.get_p(wo, k, w)?;
                    let Val::Str(vs) = v else {
                        return Err(self.type_error("Import attribute values must be strings"));
                    };
                    let ks = self.key_display(k);
                    let vv = self.rust_string(vs);
                    attributes.push((ks, vv));
                }
                attributes.sort();
            }
        }
        if self.dynamic_import_hook.is_none() {
            return Err(self.type_error("Dynamic import is not supported here"));
        }
        if self.modules.pending_live >= MAX_PENDING_IMPORTS {
            return Err(self.range_error("Too many dynamic imports are waiting to be loaded"));
        }
        let id = {
            self.modules.pending_live += 1;
            match self.modules.pending_free.pop() {
                Some(i) => {
                    self.modules.pending[i as usize] = Some(promise);
                    i
                }
                None => {
                    self.modules.pending.push(Some(promise));
                    self.modules.pending.len() as u32 - 1
                }
            }
        };
        let di = DynamicImport {
            id: DynamicImportId(id),
            referrer,
            referrer_file: file,
            request: ModuleRequest {
                specifier,
                attributes,
                line: 0,
                column: 0,
            },
        };
        let Some(mut hook) = self.dynamic_import_hook.take() else {
            return Ok(());
        };
        let r = hook(self, di);
        if self.dynamic_import_hook.is_none() {
            self.dynamic_import_hook = Some(hook);
        }
        match r {
            Ok(()) => Ok(()),
            Err(e) => {
                self.pending_release(id);
                Err(e)
            }
        }
    }

    pub(crate) fn dynamic_import_finish(
        &mut self,
        id: DynamicImportId,
        result: Result<ModuleId, Val>,
    ) -> Res<()> {
        let Some(slot) = self.modules.pending.get_mut(id.0 as usize) else {
            return Ok(());
        };
        let Some(promise) = slot.take() else {
            return Ok(());
        };
        self.modules.pending_live -= 1;
        self.modules.pending_free.push(id.0);
        let e = Val::Str(self.nstr(N::empty));
        let (arg, ok) = match result {
            Ok(m) => (pack(m), true),
            Err(v) => (v, false),
        };
        let f = self.new_builtin_slots(
            e,
            0,
            continue_dynamic_import,
            alloc::vec![Val::Obj(promise), arg, Val::Bool(ok)],
        );
        self.push_call_job(Val::Obj(f), Vec::new());
        Ok(())
    }

    pub(crate) fn dynamic_import_count(&self) -> usize {
        self.modules.pending_live
    }

    fn pending_release(&mut self, id: u32) {
        if let Some(slot) = self.modules.pending.get_mut(id as usize)
            && slot.take().is_some()
        {
            self.modules.pending_live -= 1;
            self.modules.pending_free.push(id);
        }
    }

    pub(crate) fn dynamic_import_cancel(&mut self) {
        self.modules.pending.clear();
        self.modules.pending_free.clear();
        self.modules.pending_live = 0;
    }
}

// ---- native callbacks ----

fn callee_slot(r: &Realm, i: usize) -> Val {
    r.native_slot(r.cur_callee(), i)
}

fn async_module_fulfilled(r: &mut Realm, _this: Val, _args: &[Val]) -> Res<Val> {
    if let Some(id) = unpack(callee_slot(r, 0))
        && r.modules.valid(id)
    {
        r.async_fulfilled(id.0 as usize)?;
    }
    Ok(Val::Undef)
}

fn async_module_rejected(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    if let Some(id) = unpack(callee_slot(r, 0))
        && r.modules.valid(id)
    {
        let err = args.first().copied().unwrap_or(Val::Undef);
        r.async_rejected(id.0 as usize, err)?;
    }
    Ok(Val::Undef)
}

/// The job that carries on an `import()` once the host has the module (or an error).
fn continue_dynamic_import(r: &mut Realm, _this: Val, _args: &[Val]) -> Res<Val> {
    let Val::Obj(promise) = callee_slot(r, 0) else {
        return Ok(Val::Undef);
    };
    let arg = callee_slot(r, 1);
    let ok = matches!(callee_slot(r, 2), Val::Bool(true));
    if !ok {
        r.reject_promise(promise, arg);
        return Ok(Val::Undef);
    }
    let Some(id) = unpack(arg) else {
        return Ok(Val::Undef);
    };
    if !r.modules.valid(id) {
        let e = r.make_error(crate::api::ErrorKind::TypeError, "the module was dropped");
        r.reject_promise(promise, Val::Obj(e));
        return Ok(Val::Undef);
    }
    let mi = id.0 as usize;
    // Link (if needed) and evaluate; any error rejects the import's promise.
    let step: Res<Val> = (|| {
        if r.mrec(mi).status == St::Unlinked {
            r.link_idx(mi)?;
        }
        r.evaluate_idx(mi)
    })();
    let ep = match step {
        Ok(Val::Obj(p)) => p,
        Ok(_) => return Ok(Val::Undef),
        Err(Exception(Exc::Throw(e))) => {
            r.reject_promise(promise, e);
            return Ok(Val::Undef);
        }
        Err(e) => return Err(e),
    };
    let e = Val::Str(r.nstr(N::empty));
    let ok_fn = r.new_builtin_slots(
        e,
        1,
        dynamic_import_fulfilled,
        alloc::vec![Val::Obj(promise), pack(id)],
    );
    let bad_fn = r.new_builtin_slots(
        e,
        1,
        dynamic_import_rejected,
        alloc::vec![Val::Obj(promise)],
    );
    r.perform_then(
        ep,
        crate::builtins::promise::Handler::Js(Val::Obj(ok_fn)),
        crate::builtins::promise::Handler::Js(Val::Obj(bad_fn)),
        None,
    );
    Ok(Val::Undef)
}

fn dynamic_import_fulfilled(r: &mut Realm, _this: Val, _args: &[Val]) -> Res<Val> {
    let Val::Obj(promise) = callee_slot(r, 0) else {
        return Ok(Val::Undef);
    };
    let Some(id) = unpack(callee_slot(r, 1)) else {
        return Ok(Val::Undef);
    };
    if !r.modules.valid(id) {
        let e = r.make_error(crate::api::ErrorKind::TypeError, "the module was dropped");
        r.reject_promise(promise, Val::Obj(e));
        return Ok(Val::Undef);
    }
    match r.get_module_namespace(id.0 as usize) {
        Ok(ns) => r.resolve_promise(promise, Val::Obj(ns))?,
        Err(Exception(Exc::Throw(e))) => r.reject_promise(promise, e),
        Err(e) => return Err(e),
    }
    Ok(Val::Undef)
}

fn dynamic_import_rejected(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    if let Val::Obj(promise) = callee_slot(r, 0) {
        let err = args.first().copied().unwrap_or(Val::Undef);
        r.reject_promise(promise, err);
    }
    Ok(Val::Undef)
}

impl crate::ast::ImportName {
    pub(crate) fn clone_name(&self) -> ImportName {
        match self {
            ImportName::Name(n) => ImportName::Name(n.clone()),
            ImportName::All => ImportName::All,
        }
    }
}

#[allow(dead_code)]
fn _unused(_: BTreeMap<u32, u32>) {}
