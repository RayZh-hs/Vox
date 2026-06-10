use std::collections::BTreeMap;

use vox_core::{
    mir::{
        MirBlock, MirBlockId, MirBody, MirBodyKind, MirModule, MirOpKind, MirPathSegment,
        MirProjection, MirTerminator, MirValueId,
    },
    plan::WasmArtifact,
    value::InlineValue,
};

#[derive(Debug, Default)]
pub(crate) struct WasmBackend;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WasmLowering {
    Lowered(WasmArtifact),
    Unsupported(String),
}

const TAG_INT: i32 = 0;
const TAG_FLOAT: i32 = 1;
const TAG_BOOL: i32 = 2;
const TAG_TUPLE: i32 = 4;
const TAG_HANDLE: i32 = 7;
const TAG_NULL: i32 = 8;

const SCRATCH_OFF: u32 = 0;
const RESULT_OFF: u32 = 16384;
const STRDATA_OFF: u32 = 32768;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
enum BuiltinOp {
    TupleNew = 0,
    RecordNew = 1,
    ListNew = 2,
    StringNew = 3,
    StringInterpolate = 4,
    Project = 5,
    Index = 6,
    Updated = 7,
    TypeTest = 8,
    Iterator = 9,
    IteratorNext = 10,
    LambdaNew = 11,
    EconNew = 12,
    NonNull = 13,
    SafeProject = 14,
}

struct Ctx {
    value_index: BTreeMap<MirValueId, u32>,
    block_index: BTreeMap<MirBlockId, u32>,
    string_data: Vec<u8>,
    string_offsets: BTreeMap<Vec<u8>, u32>,
    temp_count: u32,
}

impl Ctx {
    fn new(body: &MirBody, _module: &MirModule) -> Self {
        let mut value_index = BTreeMap::new();
        let mut idx = 0u32;
        for v in &body.values {
            if !value_index.contains_key(&v.id) {
                value_index.insert(v.id, idx);
                idx += 1;
            }
        }
        for p in &body.parameters {
            if !value_index.contains_key(p) {
                value_index.insert(*p, idx);
                idx += 1;
            }
        }
        let mut block_index = BTreeMap::new();
        for (i, b) in body.blocks.iter().enumerate() {
            block_index.insert(b.id, i as u32);
        }

        Self {
            value_index,
            block_index,
            string_data: Vec::new(),
            string_offsets: BTreeMap::new(),
            temp_count: 0,
        }
    }

    fn intern_string(&mut self, s: &[u8]) -> u32 {
        if let Some(&off) = self.string_offsets.get(s) {
            return off;
        }
        let off = STRDATA_OFF + self.string_data.len() as u32;
        self.string_data.extend_from_slice(&(s.len() as u32).to_le_bytes());
        self.string_data.extend_from_slice(s);
        self.string_offsets.insert(s.to_vec(), off);
        off
    }

    fn tag_local(&self, vid: MirValueId) -> u32 {
        self.value_index.get(&vid).copied().unwrap_or(0) * 2 + 1
    }

    fn data_local(&self, vid: MirValueId) -> u32 {
        self.value_index.get(&vid).copied().unwrap_or(0) * 2 + 2
    }

    fn num_value_locals(&self) -> u32 {
        self.value_index.len() as u32 * 2
    }

    fn block_id_local(&self) -> u32 {
        self.num_value_locals() + self.temp_count * 2 + 1
    }

    fn result_tag_local(&self) -> u32 {
        self.num_value_locals() + self.temp_count * 2 + 2
    }

    fn result_data_local(&self) -> u32 {
        self.num_value_locals() + self.temp_count * 2 + 3
    }

    fn total_locals(&self) -> u32 {
        self.num_value_locals() + self.temp_count * 2 + 3
    }

    fn block_idx(&self, id: MirBlockId) -> usize {
        self.block_index.get(&id).copied().unwrap_or(0) as usize
    }

    fn alloc_temp_value(&mut self) -> MirValueId {
        let idx = self.num_value_locals() as u32 + self.temp_count as u32;
        self.temp_count += 1;
        let id = MirValueId(100000 + idx);
        self.value_index.insert(id, idx);
        id
    }
}

impl WasmBackend {
    pub(crate) fn lower(&self, module: &MirModule) -> WasmLowering {
        let Some(body) = module
            .bodies
            .iter()
            .find(|b| matches!(b.kind, MirBodyKind::ScriptEntry))
        else {
            return WasmLowering::Unsupported("missing script entry body".to_owned());
        };

        match lower_module(module, body) {
            Ok(bytes) => WasmLowering::Lowered(WasmArtifact {
                bytes,
                entry_export: "script_entry".to_owned(),
                summary: "full script-entry wasm".to_owned(),
            }),
            Err(reason) => WasmLowering::Unsupported(reason),
        }
    }
}

fn lower_module(_module: &MirModule, entry_body: &MirBody) -> Result<Vec<u8>, String> {
    let mut ctx = Ctx::new(entry_body, _module);

    let mut code = Vec::new();
    emit_body(entry_body, &mut ctx, &mut code)?;

    let mut out = Vec::new();
    module_header(&mut out);
    type_section(&mut out);
    import_section(&mut out);
    function_section(&mut out);
    export_section(&mut out);
    if !ctx.string_data.is_empty() {
        data_section(&mut out, &ctx.string_data);
    }
    code_section(&mut out, &code);
    Ok(out)
}

fn module_header(out: &mut Vec<u8>) {
    out.extend_from_slice(b"\0asm");
    out.extend_from_slice(&1u32.to_le_bytes());
}

fn type_section(out: &mut Vec<u8>) {
    let mut p = Vec::new();
    write_uleb_u32(&mut p, 3);

    func_type(&mut p, &[0x7f, 0x7f, 0x7f, 0x7f, 0x7f, 0x7f], &[]);
    func_type(&mut p, &[0x7f, 0x7f, 0x7f, 0x7f, 0x7f], &[]);
    func_type(&mut p, &[0x7f], &[0x7f, 0x7e]);

    sec(out, 1, &p);
}

fn func_type(out: &mut Vec<u8>, params: &[u8], results: &[u8]) {
    out.push(0x60);
    write_uleb_u32(out, params.len() as u32);
    out.extend_from_slice(params);
    write_uleb_u32(out, results.len() as u32);
    out.extend_from_slice(results);
}

fn import_section(out: &mut Vec<u8>) {
    let mut p = Vec::new();
    write_uleb_u32(&mut p, 3);

    import_memory(&mut p, "vox", "memory", 1);
    import_func(&mut p, "vox", "__vox_op", 0);
    import_func(&mut p, "vox", "__vox_host", 1);

    sec(out, 2, &p);
}

fn import_func(out: &mut Vec<u8>, module: &str, name: &str, type_idx: u32) {
    write_name(out, module);
    write_name(out, name);
    out.push(0x00);
    write_uleb_u32(out, type_idx);
}

fn import_memory(out: &mut Vec<u8>, module: &str, name: &str, min_pages: u32) {
    write_name(out, module);
    write_name(out, name);
    out.push(0x02);
    encode_limits(out, min_pages, None);
}

fn encode_limits(out: &mut Vec<u8>, min: u32, max: Option<u32>) {
    match max {
        Some(m) => {
            out.push(0x01);
            write_uleb_u32(out, min);
            write_uleb_u32(out, m);
        }
        None => {
            out.push(0x00);
            write_uleb_u32(out, min);
        }
    }
}

fn function_section(out: &mut Vec<u8>) {
    let mut p = Vec::new();
    write_uleb_u32(&mut p, 1);
    write_uleb_u32(&mut p, 2);
    sec(out, 3, &p);
}

fn export_section(out: &mut Vec<u8>) {
    let mut p = Vec::new();
    write_uleb_u32(&mut p, 2);

    write_name(&mut p, "script_entry");
    p.push(0x00);
    write_uleb_u32(&mut p, 2);

    write_name(&mut p, "memory");
    p.push(0x02);
    write_uleb_u32(&mut p, 0);

    sec(out, 7, &p);
}

fn data_section(out: &mut Vec<u8>, data: &[u8]) {
    let mut seg = Vec::new();
    seg.push(0x00);
    seg.push(0x41);
    write_sleb_i32(&mut seg, STRDATA_OFF as i32);
    seg.push(0x0b);
    write_uleb_u32(&mut seg, data.len() as u32);
    seg.extend_from_slice(data);

    let mut p = Vec::new();
    write_uleb_u32(&mut p, 1);
    p.extend_from_slice(&seg);
    sec(out, 11, &p);
}

fn code_section(out: &mut Vec<u8>, body: &[u8]) {
    let mut p = Vec::new();
    write_uleb_u32(&mut p, 1);
    write_uleb_u32(&mut p, body.len() as u32);
    p.extend_from_slice(body);
    sec(out, 10, &p);
}

fn emit_body(body: &MirBody, ctx: &mut Ctx, code: &mut Vec<u8>) -> Result<(), String> {
    let total = ctx.total_locals() as usize;
    let mut groups: Vec<(u32, u8)> = Vec::new();
    for i in 0..total {
        let ty: u8 = if i >= total.saturating_sub(2) && i < total.saturating_sub(1) {
            0x7f
        } else if i == total.saturating_sub(1) {
            0x7e
        } else if i % 2 == 0 {
            0x7f
        } else {
            0x7e
        };
        if let Some((cnt, prev)) = groups.last_mut() {
            if *prev == ty {
                *cnt += 1;
                continue;
            }
        }
        groups.push((1, ty));
    }
    write_uleb_u32(code, groups.len() as u32);
    for (cnt, ty) in groups {
        write_uleb_u32(code, cnt);
        code.push(ty);
    }

    for (i, param) in body.parameters.iter().enumerate() {
        let tag = ctx.tag_local(*param);
        let data = ctx.data_local(*param);
        code.push(0x20);
        write_uleb_u32(code, 0);
        i32_const(code, (4 + i as u32 * 16) as i32)?;
        code.push(0x6a);
        code.push(0x28);
        write_uleb_u32(code, 0);
        write_uleb_u32(code, 0);
        local_set(code, tag)?;
        code.push(0x20);
        write_uleb_u32(code, 0);
        i32_const(code, (8 + i as u32 * 16) as i32)?;
        code.push(0x6a);
        code.push(0x29);
        write_uleb_u32(code, 0);
        write_uleb_u32(code, 0);
        local_set(code, data)?;
    }

    let eid = ctx.block_idx(body.blocks.first().map(|b| b.id).unwrap_or(MirBlockId(0)));
    i32_const(code, eid as i32)?;
    local_set(code, ctx.block_id_local())?;

    code.push(0x02);
    code.push(0x40);

    code.push(0x03);
    code.push(0x40);

    let blocks: Vec<(usize, MirBlock)> = body
        .blocks
        .iter()
        .map(|b| (ctx.block_idx(b.id), b.clone()))
        .collect();

    for (block_idx, block) in &blocks {
        i32_const(code, *block_idx as i32)?;
        local_get(code, ctx.block_id_local())?;
        code.push(0x46);
        code.push(0x04);
        code.push(0x40);

        for op in &block.ops {
            emit_op(&op.kind, &op.args, op.result, ctx, code, body)?;
        }

        match &block.terminator {
            MirTerminator::Jump { target, args } => {
                bind_block_args(body, *target, args, ctx, code)?;
                i32_const(code, ctx.block_idx(*target) as i32)?;
                local_set(code, ctx.block_id_local())?;
                br(code, 1)?;
            }
            MirTerminator::Branch {
                condition,
                then_target,
                then_args,
                else_target,
                else_args,
            } => {
                let t_i = ctx.block_idx(*then_target);
                let e_i = ctx.block_idx(*else_target);
                local_get(code, ctx.tag_local(*condition))?;
                i32_const(code, TAG_BOOL)?;
                code.push(0x46);
                code.push(0x04);
                code.push(0x40);
                bind_block_args(body, *then_target, then_args, ctx, code)?;
                i32_const(code, t_i as i32)?;
                local_set(code, ctx.block_id_local())?;
                br(code, 1)?;
                code.push(0x05);
                bind_block_args(body, *else_target, else_args, ctx, code)?;
                i32_const(code, e_i as i32)?;
                local_set(code, ctx.block_id_local())?;
                br(code, 1)?;
                code.push(0x0b);
            }
            MirTerminator::Return(value) => {
                local_get(code, ctx.tag_local(*value))?;
                local_set(code, ctx.result_tag_local())?;
                local_get(code, ctx.data_local(*value))?;
                local_set(code, ctx.result_data_local())?;
                br(code, 2)?;
            }
            MirTerminator::Panic(_) | MirTerminator::Unreachable => {
                code.push(0x00);
            }
        }

        code.push(0x0b);
    }

    br(code, 1)?;
    code.push(0x0b);
    code.push(0x0b);

    local_get(code, ctx.result_tag_local())?;
    local_get(code, ctx.result_data_local())?;
    code.push(0x0b);
    Ok(())
}

fn bind_block_args(
    body: &MirBody,
    target: MirBlockId,
    args: &[MirValueId],
    ctx: &Ctx,
    code: &mut Vec<u8>,
) -> Result<(), String> {
    let block = body
        .blocks
        .iter()
        .find(|b| b.id == target)
        .ok_or_else(|| format!("block %bb{} not found", target.0))?;
    for (param, arg) in block.parameters.iter().zip(args) {
        local_get(code, ctx.tag_local(*arg))?;
        local_set(code, ctx.tag_local(*param))?;
        local_get(code, ctx.data_local(*arg))?;
        local_set(code, ctx.data_local(*param))?;
    }
    Ok(())
}

fn emit_op(
    kind: &MirOpKind,
    args: &[MirValueId],
    result: Option<MirValueId>,
    ctx: &mut Ctx,
    code: &mut Vec<u8>,
    _body: &MirBody,
) -> Result<(), String> {
    match kind {
        MirOpKind::Literal(val) => emit_literal(val, result, ctx, code)?,
        MirOpKind::Unit => {
            if let Some(rid) = result {
                i32_const(code, TAG_TUPLE)?;
                local_set(code, ctx.tag_local(rid))?;
                i64_const(code, 0)?;
                local_set(code, ctx.data_local(rid))?;
            }
        }
        MirOpKind::Use(_) | MirOpKind::TypeRefine(_) => {
            if let (Some(rid), Some(&arg)) = (result, args.first()) {
                local_get(code, ctx.tag_local(arg))?;
                local_set(code, ctx.tag_local(rid))?;
                local_get(code, ctx.data_local(arg))?;
                local_set(code, ctx.data_local(rid))?;
            }
        }
        MirOpKind::Bind(_) | MirOpKind::CacheGet(_) | MirOpKind::CachePut(_) | MirOpKind::Drop => {}
        MirOpKind::NonNull => {
            if let (Some(rid), Some(&arg)) = (result, args.first()) {
                builtin_op(BuiltinOp::NonNull, &[arg], &[], rid, ctx, code)?;
            }
        }
        MirOpKind::SafeProject(field) => {
            if let (Some(rid), Some(&arg)) = (result, args.first()) {
                let f = field.as_bytes().to_vec();
                builtin_op(BuiltinOp::SafeProject, &[arg], &[f], rid, ctx, code)?;
            }
        }
        MirOpKind::TypeTest(ty) => {
            if let (Some(rid), Some(&arg)) = (result, args.first()) {
                let t = ty.as_bytes().to_vec();
                builtin_predicate(BuiltinOp::TypeTest, &[arg], &[t], rid, ctx, code)?;
            }
        }
        MirOpKind::Unary(name) => {
            if let (Some(rid), Some(&arg)) = (result, args.first()) {
                emit_unary(name, arg, rid, ctx, code)?;
            }
        }
        MirOpKind::Binary(name) => {
            let s: &[MirValueId] = args;
            if let (Some(rid), [left, right]) = (result, s) {
                emit_binary(name, *left, *right, rid, ctx, code)?;
            }
        }
        MirOpKind::Tuple { .. } => {
            if let Some(rid) = result {
                builtin_op(BuiltinOp::TupleNew, args, &[], rid, ctx, code)?;
            }
        }
        MirOpKind::Record { fields } => {
            if let Some(rid) = result {
                let names: Vec<Vec<u8>> = fields.iter().map(|f| f.as_bytes().to_vec()).collect();
                builtin_op(BuiltinOp::RecordNew, args, &names, rid, ctx, code)?;
            }
        }
        MirOpKind::List => {
            if let Some(rid) = result {
                builtin_op(BuiltinOp::ListNew, args, &[], rid, ctx, code)?;
            }
        }
        MirOpKind::StringInterpolate { text } => {
            if let Some(rid) = result {
                let segs: Vec<Vec<u8>> = text.iter().map(|s| s.as_bytes().to_vec()).collect();
                builtin_op(BuiltinOp::StringInterpolate, args, &segs, rid, ctx, code)?;
            }
        }
        MirOpKind::Project(proj) => {
            if let (Some(rid), Some(&arg)) = (result, args.first()) {
                match proj {
                    MirProjection::Field(f) => {
                        let mut d = vec![0u8];
                        d.extend_from_slice(&(f.len() as u32).to_le_bytes());
                        d.extend_from_slice(f.as_bytes());
                        builtin_op(BuiltinOp::Project, &[arg], &[d], rid, ctx, code)?;
                    }
                    MirProjection::Slot(s) => {
                        let mut d = vec![1u8];
                        d.extend_from_slice(&(*s as u32).to_le_bytes());
                        builtin_op(BuiltinOp::Project, &[arg], &[d], rid, ctx, code)?;
                    }
                }
            }
        }
        MirOpKind::Index => {
            let s: &[MirValueId] = args;
            if let (Some(rid), [tgt, idx]) = (result, s) {
                builtin_op(BuiltinOp::Index, &[*tgt, *idx], &[], rid, ctx, code)?;
            }
        }
        MirOpKind::Updated { path } => {
            let s: &[MirValueId] = args;
            if let (Some(rid), [tgt, repl]) = (result, s) {
                let mut pd = Vec::new();
                pd.extend_from_slice(&(path.len() as u32).to_le_bytes());
                for seg in path {
                    match seg {
                        MirPathSegment::Field(f) => {
                            pd.push(0);
                            pd.extend_from_slice(&(f.len() as u32).to_le_bytes());
                            pd.extend_from_slice(f.as_bytes());
                        }
                        MirPathSegment::Index(i) => {
                            pd.push(1);
                            pd.extend_from_slice(&(*i as u32).to_le_bytes());
                        }
                    }
                }
                builtin_op(BuiltinOp::Updated, &[*tgt, *repl], &[pd], rid, ctx, code)?;
            }
        }
        MirOpKind::Call { callee, .. } => {
            if let Some(rid) = result {
                emit_host_call(callee, args, rid, ctx, code)?;
            }
        }
        MirOpKind::Lambda { parameters } => {
            if let Some(rid) = result {
                let ps: Vec<Vec<u8>> = parameters.iter().map(|p| p.as_bytes().to_vec()).collect();
                builtin_op(BuiltinOp::LambdaNew, &[], &ps, rid, ctx, code)?;
            }
        }
        MirOpKind::Econ { .. } => {
            if let (Some(rid), Some(&arg)) = (result, args.first()) {
                builtin_op(BuiltinOp::EconNew, &[arg], &[], rid, ctx, code)?;
            }
        }
        MirOpKind::Iterator => {
            if let (Some(rid), Some(&arg)) = (result, args.first()) {
                builtin_op(BuiltinOp::Iterator, &[arg], &[], rid, ctx, code)?;
            }
        }
        MirOpKind::IteratorNext => {
            if let (Some(rid), Some(&arg)) = (result, args.first()) {
                builtin_op(BuiltinOp::IteratorNext, &[arg], &[], rid, ctx, code)?;
            }
        }
        MirOpKind::Unknown(_) => return Err("unknown MIR op".to_owned()),
    }
    Ok(())
}

fn emit_literal(
    val: &InlineValue,
    result: Option<MirValueId>,
    ctx: &mut Ctx,
    code: &mut Vec<u8>,
) -> Result<(), String> {
    let rid = match result {
        Some(r) => r,
        None => return Ok(()),
    };
    match val {
        InlineValue::Int(v) => {
            i32_const(code, TAG_INT)?;
            local_set(code, ctx.tag_local(rid))?;
            i64_const(code, *v)?;
            local_set(code, ctx.data_local(rid))?;
        }
        InlineValue::Float(v) => {
            i32_const(code, TAG_FLOAT)?;
            local_set(code, ctx.tag_local(rid))?;
            code.push(0x44);
            code.extend_from_slice(&v.to_le_bytes());
            local_set(code, ctx.data_local(rid))?;
        }
        InlineValue::Bool(v) => {
            i32_const(code, TAG_BOOL)?;
            local_set(code, ctx.tag_local(rid))?;
            i64_const(code, *v as i64)?;
            local_set(code, ctx.data_local(rid))?;
        }
        InlineValue::String(s) => {
            let b = s.as_bytes().to_vec();
            let off = ctx.intern_string(&b);
            i32_const(code, BuiltinOp::StringNew as i32)?;
            i32_const(code, off as i32)?;
            i32_const(code, b.len() as i32)?;
            i32_const(code, RESULT_OFF as i32)?;
            call_func(code, 0)?;
            i32_load(code, RESULT_OFF)?;
            local_set(code, ctx.tag_local(rid))?;
            i64_load(code, RESULT_OFF + 8)?;
            local_set(code, ctx.data_local(rid))?;
        }
        InlineValue::Tuple(items) => {
            let mut temp_ids = Vec::new();
            for item in items.iter() {
                let tid = ctx.alloc_temp_value();
                emit_literal(item, Some(tid), ctx, code)?;
                temp_ids.push(tid);
            }
            builtin_op(BuiltinOp::TupleNew, &temp_ids, &[], rid, ctx, code)?;
        }
        InlineValue::Null => {
            i32_const(code, TAG_NULL)?;
            local_set(code, ctx.tag_local(rid))?;
            i64_const(code, 0)?;
            local_set(code, ctx.data_local(rid))?;
        }
        InlineValue::Handle(_) => {
            i32_const(code, TAG_HANDLE)?;
            local_set(code, ctx.tag_local(rid))?;
            i64_const(code, 0)?;
            local_set(code, ctx.data_local(rid))?;
        }
        InlineValue::Record(fields) => {
            let mut temp_ids = Vec::new();
            let mut names: Vec<Vec<u8>> = Vec::new();
            for (name, value) in fields {
                names.push(name.as_bytes().to_vec());
                let tid = ctx.alloc_temp_value();
                emit_literal(value, Some(tid), ctx, code)?;
                temp_ids.push(tid);
            }
            builtin_op(BuiltinOp::RecordNew, &temp_ids, &names, rid, ctx, code)?;
        }
    }
    Ok(())
}

fn emit_unary(
    name: &str,
    arg: MirValueId,
    result: MirValueId,
    ctx: &Ctx,
    code: &mut Vec<u8>,
) -> Result<(), String> {
    if name == "not" {
        i32_const(code, TAG_BOOL)?;
        local_set(code, ctx.tag_local(result))?;
        local_get(code, ctx.data_local(arg))?;
        i64_const(code, 0)?;
        code.push(0x51);
        local_set(code, ctx.data_local(result))?;
    } else if name == "negate" {
        local_get(code, ctx.tag_local(arg))?;
        i32_const(code, TAG_INT)?;
        code.push(0x46);
        code.push(0x04);
        code.push(0x40);
        i32_const(code, TAG_INT)?;
        local_set(code, ctx.tag_local(result))?;
        i64_const(code, 0)?;
        local_get(code, ctx.data_local(arg))?;
        code.push(0x7d);
        local_set(code, ctx.data_local(result))?;
        br(code, 1)?;
        code.push(0x05);
        i32_const(code, TAG_FLOAT)?;
        local_set(code, ctx.tag_local(result))?;
        local_get(code, ctx.data_local(arg))?;
        code.push(0x9a);
        local_set(code, ctx.data_local(result))?;
        code.push(0x0b);
    }
    Ok(())
}

fn emit_binary(
    name: &str,
    left: MirValueId,
    right: MirValueId,
    result: MirValueId,
    ctx: &Ctx,
    code: &mut Vec<u8>,
) -> Result<(), String> {
    let cmp = [
        "equal", "not_equal", "less", "greater", "less_equal", "greater_equal",
    ];
    if cmp.contains(&name) {
        i32_const(code, TAG_BOOL)?;
        local_set(code, ctx.tag_local(result))?;
    } else {
        local_get(code, ctx.tag_local(left))?;
        local_set(code, ctx.tag_local(result))?;
    }

    match name {
        "add" | "subtract" | "multiply" | "divide" | "remainder" => {
            let opc: u8 = match name {
                "add" => 0x7c,
                "subtract" => 0x7d,
                "multiply" => 0x7e,
                "divide" => 0x7f,
                _ => 0x7e,
            };
            local_get(code, ctx.data_local(left))?;
            local_get(code, ctx.data_local(right))?;
            code.push(opc);
            local_set(code, ctx.data_local(result))?;
        }
        "equal" => eq_cmp(left, right, result, false, ctx, code)?,
        "not_equal" => eq_cmp(left, right, result, true, ctx, code)?,
        "less" | "greater" | "less_equal" | "greater_equal" => {
            cmp_op(left, right, result, name, ctx, code)?;
        }
        _ => {}
    }
    Ok(())
}

fn eq_cmp(
    left: MirValueId,
    right: MirValueId,
    result: MirValueId,
    negate: bool,
    ctx: &Ctx,
    code: &mut Vec<u8>,
) -> Result<(), String> {
    local_get(code, ctx.tag_local(left))?;
    local_get(code, ctx.tag_local(right))?;
    code.push(0x46);
    code.push(0x04);
    code.push(0x40);
    local_get(code, ctx.tag_local(left))?;
    i32_const(code, TAG_INT)?;
    code.push(0x46);
    code.push(0x04);
    code.push(0x40);
    local_get(code, ctx.data_local(left))?;
    local_get(code, ctx.data_local(right))?;
    code.push(0x51);
    code.push(0x05);
    local_get(code, ctx.data_local(left))?;
    local_get(code, ctx.data_local(right))?;
    code.push(0x61);
    code.push(0x0b);
    code.push(0x05);
    i64_const(code, 0)?;
    code.push(0x0b);
    if negate {
        code.push(0x45);
    }
    to_i64(code)?;
    local_set(code, ctx.data_local(result))?;
    Ok(())
}

fn cmp_op(
    left: MirValueId,
    right: MirValueId,
    result: MirValueId,
    op: &str,
    ctx: &Ctx,
    code: &mut Vec<u8>,
) -> Result<(), String> {
    local_get(code, ctx.tag_local(left))?;
    i32_const(code, TAG_INT)?;
    code.push(0x46);
    code.push(0x04);
    code.push(0x40);
    local_get(code, ctx.data_local(left))?;
    local_get(code, ctx.data_local(right))?;
    match op {
        "less" => code.push(0x53),
        "greater" => code.push(0x55),
        "less_equal" => code.push(0x57),
        "greater_equal" => code.push(0x59),
        _ => {}
    }
    to_i64(code)?;
    local_set(code, ctx.data_local(result))?;
    br(code, 1)?;
    code.push(0x05);
    i64_const(code, 0)?;
    local_set(code, ctx.data_local(result))?;
    code.push(0x0b);
    Ok(())
}

fn builtin_op(
    op: BuiltinOp,
    args: &[MirValueId],
    extra: &[Vec<u8>],
    result: MirValueId,
    ctx: &mut Ctx,
    code: &mut Vec<u8>,
) -> Result<(), String> {
    for (i, arg) in args.iter().enumerate() {
        i32_store_at(code, SCRATCH_OFF + i as u32 * 16, ctx.tag_local(*arg))?;
        i64_store_at(code, SCRATCH_OFF + i as u32 * 16 + 8, ctx.data_local(*arg))?;
    }

    let extra_scratch = 8192u32;
    let mut extra_scratch_pos = extra_scratch;
    for chunk in extra {
        let off = ctx.intern_string(chunk);
        i32_store32(code, extra_scratch_pos, off as i32)?;
        i32_store32(code, extra_scratch_pos + 4, chunk.len() as i32)?;
        extra_scratch_pos += 8;
    }

    i32_const(code, op as i32)?;
    i32_const(code, SCRATCH_OFF as i32)?;
    i32_const(code, args.len() as i32)?;
    i32_const(code, if extra.is_empty() { 0 } else { extra_scratch as i32 })?;
    i32_const(code, extra.len() as i32)?;
    i32_const(code, RESULT_OFF as i32)?;
    call_func(code, 1)?;

    i32_load(code, RESULT_OFF)?;
    local_set(code, ctx.tag_local(result))?;
    i64_load(code, RESULT_OFF + 8)?;
    local_set(code, ctx.data_local(result))?;
    Ok(())
}

fn builtin_predicate(
    op: BuiltinOp,
    args: &[MirValueId],
    extra: &[Vec<u8>],
    result: MirValueId,
    ctx: &mut Ctx,
    code: &mut Vec<u8>,
) -> Result<(), String> {
    builtin_op(op, args, extra, result, ctx, code)?;
    i32_const(code, TAG_BOOL)?;
    local_set(code, ctx.tag_local(result))?;
    Ok(())
}

fn emit_host_call(
    callee: &str,
    args: &[MirValueId],
    result: MirValueId,
    ctx: &mut Ctx,
    code: &mut Vec<u8>,
) -> Result<(), String> {
    let callee_bytes = callee.as_bytes().to_vec();
    let callee_offset = ctx.intern_string(&callee_bytes);

    for (i, arg) in args.iter().enumerate() {
        i32_store_at(code, SCRATCH_OFF + i as u32 * 16, ctx.tag_local(*arg))?;
        i64_store_at(code, SCRATCH_OFF + i as u32 * 16 + 8, ctx.data_local(*arg))?;
    }

    i32_const(code, callee_offset as i32)?;
    i32_const(code, callee_bytes.len() as i32)?;
    i32_const(code, SCRATCH_OFF as i32)?;
    i32_const(code, args.len() as i32)?;
    i32_const(code, RESULT_OFF as i32)?;
    call_func(code, 1)?;

    i32_load(code, RESULT_OFF)?;
    local_set(code, ctx.tag_local(result))?;
    i64_load(code, RESULT_OFF + 8)?;
    local_set(code, ctx.data_local(result))?;
    Ok(())
}

fn i32_const(code: &mut Vec<u8>, v: i32) -> Result<(), String> {
    code.push(0x41);
    write_sleb_i32(code, v);
    Ok(())
}

fn i64_const(code: &mut Vec<u8>, v: i64) -> Result<(), String> {
    code.push(0x42);
    write_sleb_i64(code, v);
    Ok(())
}

fn local_get(code: &mut Vec<u8>, idx: u32) -> Result<(), String> {
    code.push(0x20);
    write_uleb_u32(code, idx);
    Ok(())
}

fn local_set(code: &mut Vec<u8>, idx: u32) -> Result<(), String> {
    code.push(0x21);
    write_uleb_u32(code, idx);
    Ok(())
}

fn i32_store_at(code: &mut Vec<u8>, offset: u32, val_local: u32) -> Result<(), String> {
    i32_const(code, offset as i32)?;
    local_get(code, val_local)?;
    code.push(0x36);
    write_uleb_u32(code, 0);
    write_uleb_u32(code, 0);
    Ok(())
}

fn i64_store_at(code: &mut Vec<u8>, offset: u32, val_local: u32) -> Result<(), String> {
    i32_const(code, offset as i32)?;
    local_get(code, val_local)?;
    code.push(0x37);
    write_uleb_u32(code, 0);
    write_uleb_u32(code, 0);
    Ok(())
}

fn i32_load(code: &mut Vec<u8>, offset: u32) -> Result<(), String> {
    i32_const(code, offset as i32)?;
    code.push(0x28);
    write_uleb_u32(code, 0);
    write_uleb_u32(code, 0);
    Ok(())
}

fn i64_load(code: &mut Vec<u8>, offset: u32) -> Result<(), String> {
    i32_const(code, offset as i32)?;
    code.push(0x29);
    write_uleb_u32(code, 0);
    write_uleb_u32(code, 0);
    Ok(())
}

fn i32_store32(code: &mut Vec<u8>, offset: u32, value: i32) -> Result<(), String> {
    i32_const(code, offset as i32)?;
    i32_const(code, value)?;
    code.push(0x36);
    write_uleb_u32(code, 0);
    write_uleb_u32(code, 0);
    Ok(())
}

fn br(code: &mut Vec<u8>, depth: u32) -> Result<(), String> {
    code.push(0x0c);
    write_uleb_u32(code, depth);
    Ok(())
}

fn to_i64(code: &mut Vec<u8>) -> Result<(), String> {
    code.push(0xac);
    Ok(())
}

fn call_func(code: &mut Vec<u8>, func_idx: u32) -> Result<(), String> {
    code.push(0x10);
    write_uleb_u32(code, func_idx);
    Ok(())
}

fn sec(out: &mut Vec<u8>, id: u8, data: &[u8]) {
    out.push(id);
    write_uleb_u32(out, data.len() as u32);
    out.extend_from_slice(data);
}

fn write_name(out: &mut Vec<u8>, name: &str) {
    write_uleb_u32(out, name.len() as u32);
    out.extend_from_slice(name.as_bytes());
}

fn write_uleb_u32(out: &mut Vec<u8>, mut v: u32) {
    loop {
        let mut b = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
        }
        out.push(b);
        if v == 0 {
            break;
        }
    }
}

fn write_sleb_i32(out: &mut Vec<u8>, mut v: i32) {
    loop {
        let b = (v as u8) & 0x7f;
        v >>= 7;
        let done = (v == 0 && b & 0x40 == 0) || (v == -1 && b & 0x40 != 0);
        out.push(if done { b } else { b | 0x80 });
        if done {
            break;
        }
    }
}

fn write_sleb_i64(out: &mut Vec<u8>, mut v: i64) {
    loop {
        let b = (v as u8) & 0x7f;
        v >>= 7;
        let done = (v == 0 && b & 0x40 == 0) || (v == -1 && b & 0x40 != 0);
        out.push(if done { b } else { b | 0x80 });
        if done {
            break;
        }
    }
}
