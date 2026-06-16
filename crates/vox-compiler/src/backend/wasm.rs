use std::collections::{BTreeMap, BTreeSet};

use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, DataSection, DataSegment, DataSegmentMode, EntityType,
    ExportKind, ExportSection, Function, FunctionSection, GlobalSection, GlobalType, ImportSection,
    Instruction, MemArg, MemoryType, Module, RefType, ValType,
};

use vox_core::{
    builtins,
    mir::{
        MirBlock, MirBlockId, MirBody, MirBodyId, MirBodyKind, MirModule, MirOpKind,
        MirPathSegment, MirProjection, MirTerminator, MirValue, MirValueDefinition, MirValueId,
        MirVersionId, MirVersionSource,
    },
    plan::WasmArtifact,
    types::VoxType,
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
const TAG_STRING: i32 = 3;
const TAG_TUPLE: i32 = 4;
const TAG_RECORD: i32 = 5;
const TAG_LIST: i32 = 6;
const TAG_HANDLE: i32 = 7;
const TAG_NULL: i32 = 8;
const TAG_CLOSURE: i32 = 9;

const SCRATCH_OFF: u32 = 0;
const RESULT_OFF: u32 = 16384;
const STRDATA_OFF: u32 = 32768;
const WASM_PAGE_SIZE: u32 = 65536;
const HEAP_GUARD_BYTES: u32 = 4096;
const INITIAL_MEMORY_PAGES: u32 = 256;
const HEAP_LIMIT: u32 = INITIAL_MEMORY_PAGES * WASM_PAGE_SIZE - HEAP_GUARD_BYTES;
const HEAP_TOP_GLOBAL: u32 = 0;

// Linear-memory complex value layouts:
// String: [u32 byte_len][u8 bytes...]
// Tuple:  [u32 count][(tag i32, padding i32, data i64)...]
// List:   [u32 count][(tag i32, padding i32, data i64)...]
// Record: [u32 field_count][(u32 name_len, u8 name..., tag i32, data i64)...]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
#[allow(dead_code)]
enum BuiltinOp {
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
    StringBinary = 15,
    NumericChecked = 16,
    RangeNew = 17,
    HeapExhausted = 18,
    BuiltinMethod = 19,
}

struct Ctx {
    parameter_index: BTreeMap<MirValueId, u32>,
    value_index: BTreeMap<MirValueId, u32>,
    block_index: BTreeMap<MirBlockId, u32>,
    string_data: Vec<u8>,
    string_offsets: BTreeMap<Vec<u8>, u32>,
    temp_count: u32,
    func_map: BTreeMap<String, u32>,
    version_index: BTreeMap<MirVersionId, u32>,
    version_count: u32,
    version_to_binding_value: BTreeMap<MirVersionId, MirValueId>,
    known_tags: BTreeMap<MirValueId, i32>,
    lambda_table: BTreeMap<MirBodyId, (u32, usize)>,
    lambda_types: BTreeMap<usize, u32>,
    is_lambda_body: bool,
}

impl Ctx {
    fn new(body: &MirBody, _module: &MirModule) -> Self {
        let mut ctx = Self {
            parameter_index: BTreeMap::new(),
            value_index: BTreeMap::new(),
            block_index: BTreeMap::new(),
            string_data: Vec::new(),
            string_offsets: BTreeMap::new(),
            temp_count: 0,
            func_map: BTreeMap::new(),
            version_index: BTreeMap::new(),
            version_count: 0,
            version_to_binding_value: BTreeMap::new(),
            known_tags: BTreeMap::new(),
            lambda_table: BTreeMap::new(),
            lambda_types: BTreeMap::new(),
            is_lambda_body: false,
        };
        ctx.init_for_body(body);
        ctx
    }

    fn init_for_body(&mut self, body: &MirBody) {
        self.parameter_index.clear();
        self.value_index.clear();
        self.block_index.clear();
        self.temp_count = 0;
        self.version_index.clear();
        self.version_count = 0;

        self.is_lambda_body = matches!(body.kind, MirBodyKind::Lambda);
        let param_offset: u32 = if self.is_lambda_body { 1 } else { 0 };

        for (i, p) in body.parameters.iter().enumerate() {
            self.parameter_index.insert(*p, i as u32 + param_offset);
        }

        let mut idx = 0u32;
        for v in &body.values {
            if self.parameter_index.contains_key(&v.id) {
                continue;
            }
            if !self.value_index.contains_key(&v.id) {
                self.value_index.insert(v.id, idx);
                idx += 1;
            }
        }
        for (i, b) in body.blocks.iter().enumerate() {
            self.block_index.insert(b.id, i as u32);
        }

        let mut version_bind_blocks: BTreeMap<MirVersionId, BTreeSet<MirBlockId>> = BTreeMap::new();
        for block in &body.blocks {
            for op in &block.ops {
                if let MirOpKind::Bind(version) = op.kind {
                    version_bind_blocks
                        .entry(version)
                        .or_default()
                        .insert(block.id);
                }
            }
        }
        let mut rebound_versions: BTreeSet<MirVersionId> = BTreeSet::new();
        for (version, blocks) in &version_bind_blocks {
            if blocks.len() > 1 {
                rebound_versions.insert(*version);
            }
        }

        let mut vidx = 0u32;
        for version in &rebound_versions {
            self.version_index.insert(*version, vidx);
            vidx += 1;
        }
        self.version_count = vidx;

        self.version_to_binding_value.clear();
        self.known_tags.clear();

        // Seed known tags from parameter types
        for p in &body.parameters {
            if let Some(ty) = body
                .values
                .iter()
                .find(|v| v.id == *p)
                .and_then(|v| v.ty.as_ref())
            {
                if let Some(tag) = primitive_tag_for_type(ty) {
                    self.known_tags.insert(*p, tag);
                }
            }
        }

        // Seed known tags from literal values in the body
        for v in &body.values {
            if let MirValueDefinition::Literal = v.definition {
                if let Some(tag) = literal_tag(v) {
                    self.known_tags.insert(v.id, tag);
                }
            }
        }
        for binding in &body.bindings {
            if let Some(first_version) = binding.versions.first() {
                if let Some(bv) = body.versions.iter().find(|v| v.id == *first_version) {
                    let identity_value = bv.value;
                    for &version_id in &binding.versions {
                        self.version_to_binding_value
                            .insert(version_id, identity_value);
                    }
                }
            }
        }
    }

    fn intern_string(&mut self, s: &[u8]) -> u32 {
        if let Some(&off) = self.string_offsets.get(s) {
            return off;
        }
        let off = STRDATA_OFF + self.string_data.len() as u32;
        self.string_data
            .extend_from_slice(&(s.len() as u32).to_le_bytes());
        self.string_data.extend_from_slice(s);
        self.string_offsets.insert(s.to_vec(), off);
        off
    }

    fn tag_local(&self, vid: MirValueId) -> u32 {
        if let Some(idx) = self.parameter_index.get(&vid).copied() {
            idx * 2
        } else {
            self.parameter_local_count() + self.value_index.get(&vid).copied().unwrap_or(0) * 2
        }
    }

    fn data_local(&self, vid: MirValueId) -> u32 {
        if let Some(idx) = self.parameter_index.get(&vid).copied() {
            idx * 2 + 1
        } else {
            self.parameter_local_count() + self.value_index.get(&vid).copied().unwrap_or(0) * 2 + 1
        }
    }

    fn num_value_locals(&self) -> u32 {
        self.value_index.len() as u32 * 2
    }

    fn parameter_local_count(&self) -> u32 {
        let base = self.parameter_index.len() as u32 * 2;
        if self.is_lambda_body { base + 2 } else { base }
    }

    fn closure_local(&self) -> u32 {
        0
    }

    fn version_tag_local(&self, version: MirVersionId) -> u32 {
        let idx = self.version_index.get(&version).copied().unwrap_or(0);
        self.parameter_local_count() + self.num_value_locals() + self.temp_count * 2 + idx * 2
    }

    fn version_data_local(&self, version: MirVersionId) -> u32 {
        let idx = self.version_index.get(&version).copied().unwrap_or(0);
        self.parameter_local_count() + self.num_value_locals() + self.temp_count * 2 + idx * 2 + 1
    }

    fn total_locals(&self) -> u32 {
        self.num_value_locals() + self.temp_count * 2 + self.version_count * 2 + 3
    }

    fn block_id_local(&self) -> u32 {
        self.parameter_local_count()
            + self.num_value_locals()
            + self.temp_count * 2
            + self.version_count * 2
    }

    fn result_tag_local(&self) -> u32 {
        self.parameter_local_count()
            + self.num_value_locals()
            + self.temp_count * 2
            + self.version_count * 2
            + 1
    }

    fn result_data_local(&self) -> u32 {
        self.parameter_local_count()
            + self.num_value_locals()
            + self.temp_count * 2
            + self.version_count * 2
            + 2
    }

    fn block_idx(&self, id: MirBlockId) -> usize {
        self.block_index.get(&id).copied().unwrap_or(0) as usize
    }

    fn alloc_temp_value(&mut self) -> MirValueId {
        let idx = (self.num_value_locals() / 2) + self.temp_count;
        self.temp_count += 1;
        let id = MirValueId(100000 + idx);
        self.value_index.insert(id, idx);
        id
    }
}

impl WasmBackend {
    pub(crate) fn lower(&self, module: &MirModule) -> WasmLowering {
        if module.bodies.is_empty() {
            return WasmLowering::Unsupported("module has no MIR bodies".to_owned());
        }

        if let Err(reason) = validate_wasm_supported(module) {
            return WasmLowering::Unsupported(reason);
        }

        match lower_module(module) {
            Ok(bytes) => WasmLowering::Lowered(WasmArtifact {
                bytes,
                entry_export: "script_entry".to_owned(),
                summary: format!("wasm: {} bodies", module.bodies.len()),
            }),
            Err(reason) => WasmLowering::Unsupported(reason),
        }
    }
}

fn local_type_at(ctx: &Ctx, idx: u32) -> ValType {
    let local = ctx.parameter_local_count() + idx;
    if local == ctx.block_id_local() || local == ctx.result_tag_local() {
        ValType::I32
    } else if local == ctx.result_data_local() {
        ValType::I64
    } else if (local - ctx.parameter_local_count()) % 2 == 0 {
        ValType::I32
    } else {
        ValType::I64
    }
}

fn lower_module(module: &MirModule) -> Result<Vec<u8>, String> {
    let bodies: Vec<&MirBody> = module
        .bodies
        .iter()
        .filter(|b| !b.blocks.is_empty())
        .collect();

    let mut types = wasm_encoder::TypeSection::new();
    types.ty().function([ValType::I32; 6], []);
    types.ty().function([ValType::I32; 5], []);

    let mut body_type_indices: BTreeMap<usize, u32> = BTreeMap::new();
    let mut next_type_idx = 2u32;
    for body in &bodies {
        let arity = body.parameters.len();
        if body_type_indices.contains_key(&arity) {
            continue;
        }
        let mut params = Vec::with_capacity(arity * 2);
        for _ in 0..arity {
            params.push(ValType::I32);
            params.push(ValType::I64);
        }
        types.ty().function(params, [ValType::I32, ValType::I64]);
        body_type_indices.insert(arity, next_type_idx);
        next_type_idx += 1;
    }

    let mut closure_type_indices: BTreeMap<usize, u32> = BTreeMap::new();
    for body in &bodies {
        if !matches!(body.kind, MirBodyKind::Lambda) {
            continue;
        }
        let capture_count: usize = body
            .name
            .split('.')
            .nth(2)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let explicit_params = body.parameters.len().saturating_sub(capture_count);
        if closure_type_indices.contains_key(&explicit_params) {
            continue;
        }
        let mut params = Vec::with_capacity(1 + explicit_params * 2);
        params.push(ValType::I32);
        for _ in 0..explicit_params {
            params.push(ValType::I32);
            params.push(ValType::I64);
        }
        types.ty().function(params, [ValType::I32, ValType::I64]);
        closure_type_indices.insert(explicit_params, next_type_idx);
        next_type_idx += 1;
    }

    let mut imports = ImportSection::new();
    imports.import(
        "vox",
        "memory",
        EntityType::Memory(MemoryType {
            minimum: INITIAL_MEMORY_PAGES as u64,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        }),
    );
    imports.import("vox", "__vox_op", EntityType::Function(0));
    imports.import("vox", "__vox_host", EntityType::Function(1));

    let mut funcs = FunctionSection::new();

    let mut func_map: BTreeMap<String, u32> = BTreeMap::new();
    let mut func_index = 2u32;
    let mut entry_func_index = None;
    for body in &bodies {
        func_map.insert(body.name.clone(), func_index);
        func_map.insert(
            format!("{}.{}", module.module.as_str(), body.name),
            func_index,
        );
        if matches!(body.kind, MirBodyKind::ScriptEntry) {
            entry_func_index = Some(func_index);
        }
        let type_idx = *body_type_indices
            .get(&body.parameters.len())
            .ok_or_else(|| format!("missing wasm type for {} parameters", body.parameters.len()))?;
        funcs.function(type_idx);
        func_index += 1;
    }

    let entry_func_index = entry_func_index
        .ok_or_else(|| "module must have a ScriptEntry body for wasm".to_owned())?;

    let func_map_clone = func_map.clone();

    let mut exports = ExportSection::new();
    exports.export("script_entry", ExportKind::Func, entry_func_index);
    exports.export("memory", ExportKind::Memory, 0);
    exports.export("__vox_heap_top", ExportKind::Global, HEAP_TOP_GLOBAL);

    let mut ctx = Ctx::new(bodies[0], module);
    ctx.func_map = func_map;
    ctx.lambda_types = closure_type_indices;

    let lambda_count: u32 = bodies
        .iter()
        .filter(|b| matches!(b.kind, MirBodyKind::Lambda))
        .count() as u32;
    let mut table = wasm_encoder::TableSection::new();
    let mut elems = wasm_encoder::ElementSection::new();
    if lambda_count > 0 {
        table.table(wasm_encoder::TableType {
            element_type: RefType::FUNCREF,
            minimum: lambda_count as u64,
            maximum: None,
            table64: false,
            shared: false,
        });
        let mut table_index = 0u32;
        let mut elem_funcs: Vec<u32> = Vec::new();
        for body in &bodies {
            if !matches!(body.kind, MirBodyKind::Lambda) {
                continue;
            }
            let func_idx = func_map_clone
                .get(&body.name)
                .copied()
                .ok_or_else(|| format!("lambda body {} not found in func_map", body.name))?;
            elem_funcs.push(func_idx);
            let capture_count: usize = body
                .name
                .split('.')
                .nth(2)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            if let Some(item) = module.bodies.iter().find(|b| b.name == body.name) {
                ctx.lambda_table
                    .insert(item.id, (table_index, capture_count));
            }
            table_index += 1;
        }
        elems.active(
            None,
            &ConstExpr::i32_const(0),
            wasm_encoder::Elements::Functions(std::borrow::Cow::Borrowed(&elem_funcs)),
        );
    }

    let mut codes = CodeSection::new();
    for (i, body) in bodies.iter().enumerate() {
        if i > 0 {
            ctx.init_for_body(body);
        }
        let func = emit_body(body, &mut ctx)?;
        codes.function(&func);
    }

    let heap_start = align_to(STRDATA_OFF + ctx.string_data.len() as u32, 8);
    if heap_start >= HEAP_LIMIT {
        return Err(format!(
            "static wasm data ends at {heap_start}, leaving no heap space before guard"
        ));
    }

    let mut globals = GlobalSection::new();
    globals.global(
        GlobalType {
            val_type: ValType::I32,
            mutable: true,
            shared: false,
        },
        &ConstExpr::i32_const(heap_start as i32),
    );

    let mut wasm = Module::new();
    wasm.section(&types);
    wasm.section(&imports);
    wasm.section(&funcs);
    if lambda_count > 0 {
        wasm.section(&table);
        wasm.section(&elems);
    }
    wasm.section(&globals);
    wasm.section(&exports);
    wasm.section(&codes);

    if !ctx.string_data.is_empty() {
        let mut data = DataSection::new();
        let seg = DataSegment {
            mode: DataSegmentMode::Active {
                memory_index: 0,
                offset: &ConstExpr::i32_const(STRDATA_OFF as i32),
            },
            data: ctx.string_data.clone(),
        };
        data.segment(seg);
        wasm.section(&data);
    }

    Ok(wasm.finish())
}

fn validate_wasm_supported(module: &MirModule) -> Result<(), String> {
    let bodies: Vec<&MirBody> = module
        .bodies
        .iter()
        .filter(|b| !b.blocks.is_empty())
        .collect();
    let body_map = wasm_body_lookup(module, &bodies);

    if bodies.is_empty() {
        return Err("module has no executable MIR bodies".to_owned());
    }

    let has_entry = bodies
        .iter()
        .any(|b| matches!(b.kind, MirBodyKind::ScriptEntry));
    if !has_entry {
        return Err("module must have a ScriptEntry body for wasm".to_owned());
    }

    for body in &bodies {
        for value in &body.values {
            if !is_supported_wasm_value_type(value.ty.as_ref()) {
                return Err(format!(
                    "value %{} has unsupported wasm type {}",
                    value.id.0,
                    render_mir_type(value.ty.as_ref())
                ));
            }
        }
        for block in &body.blocks {
            for op in &block.ops {
                validate_wasm_op(body, &op.kind, &op.args, &body_map)?;
            }
            validate_wasm_terminator(body, &block.terminator)?;
        }
    }

    Ok(())
}

fn wasm_body_lookup<'a>(
    module: &MirModule,
    bodies: &[&'a MirBody],
) -> BTreeMap<String, &'a MirBody> {
    let mut lookup = BTreeMap::new();
    for body in bodies {
        if matches!(
            body.kind,
            MirBodyKind::Function | MirBodyKind::ValueInitializer
        ) {
            lookup.insert(body.name.clone(), *body);
            lookup.insert(format!("{}.{}", module.module.as_str(), body.name), *body);
        }
    }
    lookup
}

fn validate_wasm_op(
    body: &MirBody,
    kind: &MirOpKind,
    args: &[MirValueId],
    body_map: &BTreeMap<String, &MirBody>,
) -> Result<(), String> {
    match kind {
        MirOpKind::Literal(value) => validate_wasm_literal(value),
        MirOpKind::Use(_) | MirOpKind::TypeRefine(_) | MirOpKind::Bind(_) | MirOpKind::Drop => {
            Ok(())
        }
        MirOpKind::CacheGet(_) | MirOpKind::CachePut(_) => Ok(()),
        MirOpKind::NonNull => Ok(()),
        MirOpKind::Unit => Ok(()),
        MirOpKind::Unary(name) => match name.as_str() {
            "not" => require_args(body, args, &[WasmScalar::Bool], "not"),
            "negate" => require_numeric_args(body, args, 1, "negate", true).map(|_| ()),
            other => Err(format!("unsupported unary op `{other}` in wasm")),
        },
        MirOpKind::Binary(name) => match name.as_str() {
            "add" => validate_add_op(body, args),
            "subtract" | "multiply" | "divide" => {
                require_numeric_args(body, args, 2, name, true).map(|_| ())
            }
            "remainder" => require_numeric_args(body, args, 2, name, true).map(|_| ()),
            "less" | "greater" | "less_equal" | "greater_equal" => {
                validate_order_op(body, args, name)
            }
            "equal" | "not_equal" => {
                if args.len() != 2 {
                    return Err(format!("binary op `{name}` expected 2 args"));
                }
                let left = wasm_scalar_type(body, args[0])
                    .ok_or_else(|| format!("binary op `{name}` has unsupported left operand"))?;
                let right = wasm_scalar_type(body, args[1])
                    .ok_or_else(|| format!("binary op `{name}` has unsupported right operand"))?;
                if left == right
                    && matches!(
                        left,
                        WasmScalar::Int
                            | WasmScalar::Float
                            | WasmScalar::Bool
                            | WasmScalar::String
                            | WasmScalar::Null
                    )
                {
                    Ok(())
                } else {
                    Err(format!(
                        "binary op `{name}` is not supported for {} and {} in wasm",
                        left.as_str(),
                        right.as_str()
                    ))
                }
            }
            "range" | "range_inclusive" => {
                let count = args.len();
                if count > 2 {
                    return Err(format!("binary op `{name}` expects at most 2 args"));
                }
                require_numeric_args(body, args, count, name, false).map(|_| ())
            }
            other => Err(format!("unsupported binary op `{other}` in wasm")),
        },
        MirOpKind::TypeTest(_)
        | MirOpKind::Tuple { .. }
        | MirOpKind::Record { .. }
        | MirOpKind::List => Ok(()),
        MirOpKind::StringInterpolate { .. } => validate_string_interpolate_op(body, args),
        MirOpKind::Project(projection) => validate_projection_op(body, args, projection),
        MirOpKind::SafeProject(_) => validate_record_like_arg(body, args, "SafeProject"),
        MirOpKind::Index => validate_index_op(body, args),
        MirOpKind::Updated { .. } => validate_updated_op(body, args),
        MirOpKind::Call { callee, .. } => validate_wasm_call_op(callee, args, body_map),
        MirOpKind::Lambda { .. } => Ok(()),
        MirOpKind::Econ { .. } => Ok(()),
        MirOpKind::Iterator | MirOpKind::IteratorNext => Ok(()),
        MirOpKind::Unknown(_) => Err("unknown MIR op".to_owned()),
    }
}

fn validate_wasm_call_op(
    callee: &str,
    args: &[MirValueId],
    body_map: &BTreeMap<String, &MirBody>,
) -> Result<(), String> {
    let Some(target) = body_map.get(callee) else {
        return Ok(());
    };
    if target.parameters.len() != args.len() {
        return Err(format!(
            "function call `{callee}` expected {} args, found {}",
            target.parameters.len(),
            args.len()
        ));
    }
    if target.result_type.is_none() {
        return Err(format!(
            "function call `{callee}` has no known wasm result type"
        ));
    }
    Ok(())
}

fn validate_wasm_terminator(body: &MirBody, terminator: &MirTerminator) -> Result<(), String> {
    match terminator {
        MirTerminator::Jump { .. } | MirTerminator::Return(_) => Ok(()),
        MirTerminator::Branch { condition, .. } => {
            if wasm_scalar_type(body, *condition) == Some(WasmScalar::Bool) {
                Ok(())
            } else {
                Err("wasm branch conditions must be Bool".to_owned())
            }
        }
        MirTerminator::Panic(_) => Ok(()),
        MirTerminator::Unreachable => Ok(()),
    }
}

fn validate_wasm_literal(value: &InlineValue) -> Result<(), String> {
    match value {
        InlineValue::Int(_)
        | InlineValue::Float(_)
        | InlineValue::Bool(_)
        | InlineValue::String(_)
        | InlineValue::Null => Ok(()),
        InlineValue::Tuple(items) if items.is_empty() => Ok(()),
        InlineValue::Tuple(_) => Err("Tuple literals require handle-backed wasm data".to_owned()),
        InlineValue::Record(_) => Err("Record literals require handle-backed wasm data".to_owned()),
        InlineValue::Handle(_) => {
            Err("Handle literals cannot be materialized by wasm yet".to_owned())
        }
    }
}

fn validate_add_op(body: &MirBody, args: &[MirValueId]) -> Result<(), String> {
    if args.len() != 2 {
        return Err("op `add` expected 2 args".to_owned());
    }
    let left = wasm_scalar_type(body, args[0])
        .ok_or_else(|| "op `add` has unsupported left operand".to_owned())?;
    let right = wasm_scalar_type(body, args[1])
        .ok_or_else(|| "op `add` has unsupported right operand".to_owned())?;
    if matches!(left, WasmScalar::String) || matches!(right, WasmScalar::String) {
        if left == WasmScalar::String && right == WasmScalar::String {
            Ok(())
        } else {
            Err("string add requires both operands to be String in wasm".to_owned())
        }
    } else {
        require_numeric_args(body, args, 2, "add", true).map(|_| ())
    }
}

fn validate_order_op(body: &MirBody, args: &[MirValueId], op: &str) -> Result<(), String> {
    if args.len() != 2 {
        return Err(format!("op `{op}` expected 2 args"));
    }
    let left = wasm_scalar_type(body, args[0])
        .ok_or_else(|| format!("op `{op}` has unsupported left operand"))?;
    let right = wasm_scalar_type(body, args[1])
        .ok_or_else(|| format!("op `{op}` has unsupported right operand"))?;
    if left == WasmScalar::String || right == WasmScalar::String {
        if left == WasmScalar::String && right == WasmScalar::String {
            Ok(())
        } else {
            Err(format!(
                "string comparison `{op}` requires both operands to be String in wasm"
            ))
        }
    } else {
        require_numeric_args(body, args, 2, op, true).map(|_| ())
    }
}

fn validate_projection_op(
    body: &MirBody,
    args: &[MirValueId],
    projection: &MirProjection,
) -> Result<(), String> {
    if args.len() != 1 {
        return Err("Project expected 1 argument".to_owned());
    }
    match (value_type(body, args[0]), projection) {
        (Some(VoxType::Record(_)), MirProjection::Field(_)) => Ok(()),
        (Some(VoxType::Tuple(_)), MirProjection::Slot(_)) => Ok(()),
        (ty, _) => Err(format!(
            "Project is not supported for {} in wasm",
            render_mir_type(ty)
        )),
    }
}

fn validate_record_like_arg(body: &MirBody, args: &[MirValueId], op: &str) -> Result<(), String> {
    if args.len() != 1 {
        return Err(format!("{op} expected 1 argument"));
    }
    match value_type(body, args[0]) {
        Some(VoxType::Record(_)) => Ok(()),
        ty => Err(format!(
            "{op} is not supported for {} in wasm",
            render_mir_type(ty)
        )),
    }
}

fn validate_index_op(body: &MirBody, args: &[MirValueId]) -> Result<(), String> {
    if args.len() != 2 {
        return Err("Index expected target and index arguments".to_owned());
    }
    if wasm_scalar_type(body, args[1]) != Some(WasmScalar::Int) {
        return Err("Index requires an Int index in wasm".to_owned());
    }
    match value_type(body, args[0]) {
        Some(VoxType::Tuple(_)) | Some(VoxType::List(_)) => Ok(()),
        ty => Err(format!(
            "Index is not supported for {} in wasm",
            render_mir_type(ty)
        )),
    }
}

fn validate_updated_op(body: &MirBody, args: &[MirValueId]) -> Result<(), String> {
    if args.len() != 2 {
        return Err("Updated expected target and replacement arguments".to_owned());
    }
    match value_type(body, args[0]) {
        Some(VoxType::Record(_)) | Some(VoxType::Tuple(_)) | Some(VoxType::List(_)) => Ok(()),
        ty => Err(format!(
            "Updated is not supported for {} in wasm",
            render_mir_type(ty)
        )),
    }
}

fn validate_string_interpolate_op(body: &MirBody, args: &[MirValueId]) -> Result<(), String> {
    for arg in args {
        match value_type(body, *arg) {
            Some(VoxType::List(_)) => {
                return Err("StringInterpolate cannot render List handles in wasm yet".to_owned());
            }
            Some(
                VoxType::Int
                | VoxType::Float
                | VoxType::Bool
                | VoxType::String
                | VoxType::Tuple(_)
                | VoxType::Record(_),
            )
            | None => {}
            ty => {
                return Err(format!(
                    "StringInterpolate is not supported for {} in wasm",
                    render_mir_type(ty)
                ));
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WasmScalar {
    Int,
    Float,
    Bool,
    String,
    Null,
}

impl WasmScalar {
    fn as_str(self) -> &'static str {
        match self {
            Self::Int => "Int",
            Self::Float => "Float",
            Self::Bool => "Bool",
            Self::String => "String",
            Self::Null => "Null",
        }
    }
}

fn require_args(
    body: &MirBody,
    args: &[MirValueId],
    expected: &[WasmScalar],
    op: &str,
) -> Result<(), String> {
    if args.len() != expected.len() {
        return Err(format!("op `{op}` expected {} args", expected.len()));
    }
    for (arg, expected) in args.iter().zip(expected) {
        let Some(actual) = wasm_scalar_type(body, *arg) else {
            return Err(format!(
                "op `{op}` argument %{} has unsupported wasm type {}",
                arg.0,
                render_mir_type(value_type(body, *arg))
            ));
        };
        if actual != *expected {
            return Err(format!(
                "op `{op}` expected {}, found {}",
                expected.as_str(),
                actual.as_str()
            ));
        }
    }
    Ok(())
}

fn require_numeric_args(
    body: &MirBody,
    args: &[MirValueId],
    count: usize,
    op: &str,
    allow_mixed: bool,
) -> Result<Vec<WasmScalar>, String> {
    if args.len() != count {
        return Err(format!("op `{op}` expected {count} args"));
    }
    let mut scalars = Vec::new();
    for arg in args {
        let Some(actual) = wasm_scalar_type(body, *arg) else {
            return Err(format!(
                "op `{op}` argument %{} has unsupported wasm type {}",
                arg.0,
                render_mir_type(value_type(body, *arg))
            ));
        };
        if !matches!(actual, WasmScalar::Int | WasmScalar::Float) {
            return Err(format!(
                "op `{op}` expected numeric operand, found {}",
                actual.as_str()
            ));
        }
        scalars.push(actual);
    }
    if !allow_mixed && scalars.windows(2).any(|pair| pair[0] != pair[1]) {
        return Err(format!("op `{op}` does not support mixed numeric operands"));
    }
    Ok(scalars)
}

fn is_supported_wasm_value_type(ty: Option<&VoxType>) -> bool {
    match ty {
        None => false,
        Some(VoxType::Int | VoxType::Float | VoxType::Bool | VoxType::String) => true,
        Some(VoxType::Tuple(items)) => items
            .iter()
            .all(|item| is_supported_wasm_value_type(Some(item))),
        Some(VoxType::Record(fields)) => fields
            .iter()
            .all(|field| is_supported_wasm_value_type(Some(&field.ty))),
        Some(VoxType::List(item)) | Some(VoxType::Nullable(item)) => {
            is_supported_wasm_value_type(Some(item))
        }
        Some(VoxType::OpaqueSurface(name)) => {
            matches!(name.as_str(), "Int" | "Float" | "Bool" | "String" | "Null")
                || name.starts_with("Iterator<")
        }
        _ => false,
    }
}

fn wasm_scalar_type(body: &MirBody, value: MirValueId) -> Option<WasmScalar> {
    match value_type(body, value) {
        Some(VoxType::Int) => Some(WasmScalar::Int),
        Some(VoxType::Float) => Some(WasmScalar::Float),
        Some(VoxType::Bool) => Some(WasmScalar::Bool),
        Some(VoxType::String) => Some(WasmScalar::String),
        Some(VoxType::OpaqueSurface(name)) => match name.as_str() {
            "Int" => Some(WasmScalar::Int),
            "Float" => Some(WasmScalar::Float),
            "Bool" => Some(WasmScalar::Bool),
            "String" => Some(WasmScalar::String),
            "Null" => Some(WasmScalar::Null),
            _ => None,
        },
        None => None,
        _ => None,
    }
}

fn value_type(body: &MirBody, value: MirValueId) -> Option<&VoxType> {
    body.values
        .iter()
        .find(|candidate| candidate.id == value)
        .and_then(|candidate| candidate.ty.as_ref())
}

fn render_mir_type(ty: Option<&VoxType>) -> String {
    match ty {
        Some(ty) => format!("{ty:?}"),
        None => "unknown".to_owned(),
    }
}

#[allow(dead_code)]
fn mir_op_name(kind: &MirOpKind) -> &'static str {
    match kind {
        MirOpKind::Literal(_) => "Literal",
        MirOpKind::Unit => "Unit",
        MirOpKind::Use(_) => "Use",
        MirOpKind::Bind(_) => "Bind",
        MirOpKind::TypeRefine(_) => "TypeRefine",
        MirOpKind::CacheGet(_) => "CacheGet",
        MirOpKind::CachePut(_) => "CachePut",
        MirOpKind::Drop => "Drop",
        MirOpKind::NonNull => "NonNull",
        MirOpKind::SafeProject(_) => "SafeProject",
        MirOpKind::TypeTest(_) => "TypeTest",
        MirOpKind::Unary(_) => "Unary",
        MirOpKind::Binary(_) => "Binary",
        MirOpKind::Tuple { .. } => "Tuple",
        MirOpKind::Record { .. } => "Record",
        MirOpKind::List => "List",
        MirOpKind::StringInterpolate { .. } => "StringInterpolate",
        MirOpKind::Project(_) => "Project",
        MirOpKind::Index => "Index",
        MirOpKind::Updated { .. } => "Updated",
        MirOpKind::Call { .. } => "Call",
        MirOpKind::Lambda { .. } => "Lambda",
        MirOpKind::Econ { .. } => "Econ",
        MirOpKind::Iterator => "Iterator",
        MirOpKind::IteratorNext => "IteratorNext",
        MirOpKind::Unknown(_) => "Unknown",
    }
}

fn emit_body(body: &MirBody, ctx: &mut Ctx) -> Result<Function, String> {
    let total = ctx.total_locals() as usize;
    let mut locals: Vec<(u32, ValType)> = Vec::new();
    let mut i = 0u32;
    while i < total as u32 {
        let ty = local_type_at(ctx, i);
        let mut count = 1u32;
        while i + 1 < total as u32 && local_type_at(ctx, i + 1) == ty {
            count += 1;
            i += 1;
        }
        locals.push((count, ty));
        i += 1;
    }

    let mut f = Function::new(locals);

    if ctx.is_lambda_body {
        emit_lambda_capture_prologue(body, ctx, &mut f)?;
    }

    let entry_id = ctx.block_idx(body.blocks.first().map(|b| b.id).unwrap_or(MirBlockId(0)));
    f.instruction(&Instruction::I32Const(entry_id as i32));
    f.instruction(&Instruction::LocalSet(ctx.block_id_local()));

    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));

    let blocks: Vec<(usize, MirBlock)> = body
        .blocks
        .iter()
        .map(|b| (ctx.block_idx(b.id), b.clone()))
        .collect();

    for (block_idx, block) in &blocks {
        f.instruction(&Instruction::I32Const(*block_idx as i32));
        f.instruction(&Instruction::LocalGet(ctx.block_id_local()));
        f.instruction(&Instruction::I32Eq);
        f.instruction(&Instruction::If(BlockType::Empty));

        for op in &block.ops {
            emit_op(&op.kind, &op.args, op.result, ctx, &mut f, body)?;
        }

        match &block.terminator {
            MirTerminator::Jump { target, args } => {
                bind_block_args(body, *target, args, ctx, &mut f)?;
                f.instruction(&Instruction::I32Const(ctx.block_idx(*target) as i32));
                f.instruction(&Instruction::LocalSet(ctx.block_id_local()));
                f.instruction(&Instruction::Br(1));
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
                emit_branch_condition(*condition, ctx, &mut f);
                f.instruction(&Instruction::If(BlockType::Empty));
                bind_block_args(body, *then_target, then_args, ctx, &mut f)?;
                f.instruction(&Instruction::I32Const(t_i as i32));
                f.instruction(&Instruction::LocalSet(ctx.block_id_local()));
                f.instruction(&Instruction::Br(1));
                f.instruction(&Instruction::Else);
                bind_block_args(body, *else_target, else_args, ctx, &mut f)?;
                f.instruction(&Instruction::I32Const(e_i as i32));
                f.instruction(&Instruction::LocalSet(ctx.block_id_local()));
                f.instruction(&Instruction::Br(1));
                f.instruction(&Instruction::End);
            }
            MirTerminator::Return(value) => {
                f.instruction(&Instruction::LocalGet(ctx.tag_local(*value)));
                f.instruction(&Instruction::LocalSet(ctx.result_tag_local()));
                f.instruction(&Instruction::LocalGet(ctx.data_local(*value)));
                f.instruction(&Instruction::LocalSet(ctx.result_data_local()));
                f.instruction(&Instruction::Br(2));
            }
            MirTerminator::Panic(_) | MirTerminator::Unreachable => {
                f.instruction(&Instruction::Unreachable);
            }
        }

        f.instruction(&Instruction::End);
    }

    f.instruction(&Instruction::Br(1));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);

    f.instruction(&Instruction::LocalGet(ctx.result_tag_local()));
    f.instruction(&Instruction::LocalGet(ctx.result_data_local()));
    f.instruction(&Instruction::End);

    Ok(f)
}

fn emit_branch_condition(condition: MirValueId, ctx: &mut Ctx, f: &mut Function) {
    let check_needed = ctx.known_tags.get(&condition) != Some(&TAG_BOOL);
    if check_needed {
        local_get(f, ctx.tag_local(condition));
        i32(f, TAG_BOOL);
        f.instruction(&Instruction::I32Ne);
        f.instruction(&Instruction::If(BlockType::Empty));
        f.instruction(&Instruction::Unreachable);
        f.instruction(&Instruction::End);
    }
    local_get(f, ctx.data_local(condition));
    f.instruction(&Instruction::I64Const(0));
    f.instruction(&Instruction::I64Ne);
}

fn bind_block_args(
    body: &MirBody,
    target: MirBlockId,
    args: &[MirValueId],
    ctx: &Ctx,
    f: &mut Function,
) -> Result<(), String> {
    let block = body
        .blocks
        .iter()
        .find(|b| b.id == target)
        .ok_or_else(|| format!("block %bb{} not found", target.0))?;
    for (param, arg) in block.parameters.iter().zip(args) {
        f.instruction(&Instruction::LocalGet(ctx.tag_local(*arg)));
        f.instruction(&Instruction::LocalSet(ctx.tag_local(*param)));
        f.instruction(&Instruction::LocalGet(ctx.data_local(*arg)));
        f.instruction(&Instruction::LocalSet(ctx.data_local(*param)));
    }
    Ok(())
}

fn emit_op(
    kind: &MirOpKind,
    args: &[MirValueId],
    result: Option<MirValueId>,
    ctx: &mut Ctx,
    f: &mut Function,
    body: &MirBody,
) -> Result<(), String> {
    match kind {
        MirOpKind::Literal(val) => emit_literal(val, result, ctx, f)?,
        MirOpKind::Unit => {
            if let Some(rid) = result {
                i32(f, TAG_TUPLE);
                local_set(f, ctx.tag_local(rid));
                i64(f, 0);
                local_set(f, ctx.data_local(rid));
                record_known_tag(ctx, rid, TAG_TUPLE);
            }
        }
        MirOpKind::Use(version) => {
            if let (Some(rid), Some(&arg)) = (result, args.first()) {
                if ctx.version_index.contains_key(version) {
                    local_get(f, ctx.version_tag_local(*version));
                    local_set(f, ctx.tag_local(rid));
                    local_get(f, ctx.version_data_local(*version));
                    local_set(f, ctx.data_local(rid));
                } else {
                    local_get(f, ctx.tag_local(arg));
                    local_set(f, ctx.tag_local(rid));
                    local_get(f, ctx.data_local(arg));
                    local_set(f, ctx.data_local(rid));
                }
                if let Some(&tag) = ctx.known_tags.get(&arg) {
                    record_known_tag(ctx, rid, tag);
                }
            }
        }
        MirOpKind::TypeRefine(_) => {
            if let (Some(rid), Some(&arg)) = (result, args.first()) {
                local_get(f, ctx.tag_local(arg));
                local_set(f, ctx.tag_local(rid));
                local_get(f, ctx.data_local(arg));
                local_set(f, ctx.data_local(rid));
                if let Some(&tag) = ctx.known_tags.get(&arg) {
                    record_known_tag(ctx, rid, tag);
                }
            }
        }
        MirOpKind::Bind(version) => {
            if let Some(&value) = args.first() {
                if ctx.version_index.contains_key(version) {
                    local_get(f, ctx.tag_local(value));
                    local_set(f, ctx.version_tag_local(*version));
                    local_get(f, ctx.data_local(value));
                    local_set(f, ctx.version_data_local(*version));
                }
                if let Some(&binding_value_id) = ctx.version_to_binding_value.get(version) {
                    local_get(f, ctx.tag_local(value));
                    local_set(f, ctx.tag_local(binding_value_id));
                    local_get(f, ctx.data_local(value));
                    local_set(f, ctx.data_local(binding_value_id));
                    if let Some(&tag) = ctx.known_tags.get(&value) {
                        record_known_tag(ctx, binding_value_id, tag);
                    }
                }
            }
        }
        MirOpKind::CacheGet(_) | MirOpKind::CachePut(_) | MirOpKind::Drop => {}
        MirOpKind::NonNull => {
            if let (Some(rid), Some(&arg)) = (result, args.first()) {
                builtin_op_call(BuiltinOp::NonNull, &[arg], &[], rid, ctx, f)?;
            }
        }
        MirOpKind::SafeProject(field) => {
            if let (Some(rid), Some(&arg)) = (result, args.first()) {
                let fb = field.as_bytes().to_vec();
                builtin_op_call(BuiltinOp::SafeProject, &[arg], &[fb], rid, ctx, f)?;
            }
        }
        MirOpKind::TypeTest(ty) => {
            if let (Some(rid), Some(&arg)) = (result, args.first()) {
                let tb = ty.as_bytes().to_vec();
                builtin_predicate(BuiltinOp::TypeTest, &[arg], &[tb], rid, ctx, f)?;
            }
        }
        MirOpKind::Unary(name) => {
            if let (Some(rid), Some(&arg)) = (result, args.first()) {
                emit_unary(name, arg, rid, ctx, f, body)?;
                track_unary_result_tag(name, arg, rid, ctx, body);
            }
        }
        MirOpKind::Binary(name) => {
            let s: &[MirValueId] = args;
            match name.as_str() {
                "range" | "range_inclusive" => {
                    if let Some(rid) = result {
                        builtin_op_call(
                            BuiltinOp::RangeNew,
                            s,
                            &[name.as_bytes().to_vec()],
                            rid,
                            ctx,
                            f,
                        )?;
                    }
                }
                _ => {
                    if let (Some(rid), [left, right]) = (result, s) {
                        emit_binary(name, *left, *right, rid, ctx, f, body)?;
                        track_binary_result_tag(name, *left, rid, ctx, body);
                    }
                }
            }
        }
        MirOpKind::Tuple { .. } => {
            if let Some(rid) = result {
                emit_tuple_new(args, rid, ctx, f)?;
            }
        }
        MirOpKind::Record { fields } => {
            if let Some(rid) = result {
                let names: Vec<Vec<u8>> = fields.iter().map(|n| n.as_bytes().to_vec()).collect();
                emit_record_new(args, &names, rid, ctx, f)?;
            }
        }
        MirOpKind::List => {
            if let Some(rid) = result {
                emit_list_new(args, rid, ctx, f)?;
            }
        }
        MirOpKind::StringInterpolate { text } => {
            if let Some(rid) = result {
                let segs: Vec<Vec<u8>> = text.iter().map(|s| s.as_bytes().to_vec()).collect();
                builtin_op_call(BuiltinOp::StringInterpolate, args, &segs, rid, ctx, f)?;
            }
        }
        MirOpKind::Project(proj) => {
            if let (Some(rid), Some(&arg)) = (result, args.first()) {
                match proj {
                    MirProjection::Field(field) => {
                        let mut d = vec![0u8];
                        d.extend_from_slice(&(field.len() as u32).to_le_bytes());
                        d.extend_from_slice(field.as_bytes());
                        builtin_op_call(BuiltinOp::Project, &[arg], &[d], rid, ctx, f)?;
                    }
                    MirProjection::Slot(s) => {
                        let mut d = vec![1u8];
                        d.extend_from_slice(&(*s as u32).to_le_bytes());
                        builtin_op_call(BuiltinOp::Project, &[arg], &[d], rid, ctx, f)?;
                    }
                }
            }
        }
        MirOpKind::Index => {
            let s: &[MirValueId] = args;
            if let (Some(rid), [tgt, idx]) = (result, s) {
                builtin_op_call(BuiltinOp::Index, &[*tgt, *idx], &[], rid, ctx, f)?;
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
                builtin_op_call(BuiltinOp::Updated, &[*tgt, *repl], &[pd], rid, ctx, f)?;
            }
        }
        MirOpKind::Call { callee, .. } => {
            if let Some(rid) = result {
                if builtins::split_builtin_callee(callee).is_some() {
                    builtin_op_call(
                        BuiltinOp::BuiltinMethod,
                        args,
                        &[callee.as_bytes().to_vec()],
                        rid,
                        ctx,
                        f,
                    )?;
                } else if let Some(&target_func) = ctx.func_map.get(callee) {
                    emit_vox_call(args, target_func, rid, ctx, f)?;
                } else if callee.contains('.') {
                    emit_host_call(callee, args, rid, ctx, f)?;
                } else if let Some(callee_value) = find_binding_value(body, callee) {
                    emit_dynamic_call(callee_value, args, rid, ctx, f, body)?;
                } else {
                    emit_host_call(callee, args, rid, ctx, f)?;
                }
            }
        }
        MirOpKind::Lambda {
            parameters,
            captures,
            body_id,
        } => {
            if let Some(rid) = result {
                emit_closure_new(*body_id, captures, parameters, rid, ctx, f)?;
            }
        }
        MirOpKind::Econ { .. } => {
            if let (Some(rid), Some(&arg)) = (result, args.first()) {
                builtin_op_call(BuiltinOp::EconNew, &[arg], &[], rid, ctx, f)?;
            }
        }
        MirOpKind::Iterator => {
            if let (Some(rid), Some(&arg)) = (result, args.first()) {
                builtin_op_call(BuiltinOp::Iterator, &[arg], &[], rid, ctx, f)?;
            }
        }
        MirOpKind::IteratorNext => {
            if let (Some(rid), Some(&arg)) = (result, args.first()) {
                builtin_op_call(BuiltinOp::IteratorNext, &[arg], &[], rid, ctx, f)?;
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
    f: &mut Function,
) -> Result<(), String> {
    let rid = match result {
        Some(r) => r,
        None => return Ok(()),
    };
    match val {
        InlineValue::Int(v) => {
            i32(f, TAG_INT);
            local_set(f, ctx.tag_local(rid));
            i64(f, *v);
            local_set(f, ctx.data_local(rid));
        }
        InlineValue::Float(v) => {
            i32(f, TAG_FLOAT);
            local_set(f, ctx.tag_local(rid));
            f.instruction(&Instruction::F64Const(*v));
            f.instruction(&Instruction::I64ReinterpretF64);
            f.instruction(&Instruction::LocalSet(ctx.data_local(rid)));
        }
        InlineValue::Bool(v) => {
            i32(f, TAG_BOOL);
            local_set(f, ctx.tag_local(rid));
            i64(f, *v as i64);
            local_set(f, ctx.data_local(rid));
        }
        InlineValue::String(s) => {
            let b = s.as_bytes().to_vec();
            emit_string_new(&b, rid, ctx, f)?;
        }
        InlineValue::Tuple(items) => {
            if items.is_empty() {
                i32(f, TAG_TUPLE);
                local_set(f, ctx.tag_local(rid));
                i64(f, 0);
                local_set(f, ctx.data_local(rid));
                record_known_tag(ctx, rid, TAG_TUPLE);
            } else {
                let mut temp_ids = Vec::new();
                for item in items.iter() {
                    let tid = ctx.alloc_temp_value();
                    emit_literal(item, Some(tid), ctx, f)?;
                    temp_ids.push(tid);
                }
                emit_tuple_new(&temp_ids, rid, ctx, f)?;
            }
        }
        InlineValue::Null => {
            i32(f, TAG_NULL);
            local_set(f, ctx.tag_local(rid));
            i64(f, 0);
            local_set(f, ctx.data_local(rid));
        }
        InlineValue::Handle(_) => {
            i32(f, TAG_HANDLE);
            local_set(f, ctx.tag_local(rid));
            i64(f, 0);
            local_set(f, ctx.data_local(rid));
        }
        InlineValue::Record(fields) => {
            let mut temp_ids = Vec::new();
            let mut names: Vec<Vec<u8>> = Vec::new();
            for (name, value) in fields {
                names.push(name.as_bytes().to_vec());
                let tid = ctx.alloc_temp_value();
                emit_literal(value, Some(tid), ctx, f)?;
                temp_ids.push(tid);
            }
            emit_record_new(&temp_ids, &names, rid, ctx, f)?;
        }
    }
    Ok(())
}

fn emit_unary(
    name: &str,
    arg: MirValueId,
    result: MirValueId,
    ctx: &Ctx,
    f: &mut Function,
    body: &MirBody,
) -> Result<(), String> {
    if name == "not" {
        i32(f, TAG_BOOL);
        local_set(f, ctx.tag_local(result));
        local_get(f, ctx.data_local(arg));
        i64(f, 0);
        f.instruction(&Instruction::I64Eq);
        local_set(f, ctx.data_local(result));
    } else if name == "negate" {
        match wasm_scalar_type(body, arg) {
            Some(WasmScalar::Int) => {
                emit_tag_check(f, ctx, arg, TAG_INT);
                i32(f, TAG_INT);
                local_set(f, ctx.tag_local(result));
                i64(f, 0);
                local_get(f, ctx.data_local(arg));
                f.instruction(&Instruction::I64Sub);
                local_set(f, ctx.data_local(result));
            }
            Some(WasmScalar::Float) => {
                emit_tag_check(f, ctx, arg, TAG_FLOAT);
                i32(f, TAG_FLOAT);
                local_set(f, ctx.tag_local(result));
                emit_value_as_f64(f, ctx, arg, WasmScalar::Float);
                f.instruction(&Instruction::F64Neg);
                f.instruction(&Instruction::I64ReinterpretF64);
                local_set(f, ctx.data_local(result));
            }
            _ => return Err("negate expects Int or Float".to_owned()),
        }
    }
    Ok(())
}

fn emit_binary(
    name: &str,
    left: MirValueId,
    right: MirValueId,
    result: MirValueId,
    ctx: &mut Ctx,
    f: &mut Function,
    body: &MirBody,
) -> Result<(), String> {
    let left_ty = wasm_scalar_type(body, left);
    let right_ty = wasm_scalar_type(body, right);
    if left_ty == Some(WasmScalar::String) || right_ty == Some(WasmScalar::String) {
        if left_ty == Some(WasmScalar::String)
            && right_ty == Some(WasmScalar::String)
            && matches!(
                name,
                "add" | "equal" | "not_equal" | "less" | "greater" | "less_equal" | "greater_equal"
            )
        {
            return builtin_op_call(
                BuiltinOp::StringBinary,
                &[left, right],
                &[name.as_bytes().to_vec()],
                result,
                ctx,
                f,
            );
        }
        return Err(format!(
            "binary `{name}` is not supported for String and non-String in wasm"
        ));
    }

    let cmp = [
        "equal",
        "not_equal",
        "less",
        "greater",
        "less_equal",
        "greater_equal",
    ];
    if cmp.contains(&name) {
        i32(f, TAG_BOOL);
        local_set(f, ctx.tag_local(result));
    } else {
        local_get(f, ctx.tag_local(left));
        local_set(f, ctx.tag_local(result));
    }

    match name {
        "add" | "subtract" | "multiply" | "divide" | "remainder" => {
            emit_numeric_binary(name, left, right, result, ctx, f, body)?;
        }
        "equal" => eq_cmp(left, right, result, false, ctx, f, body)?,
        "not_equal" => eq_cmp(left, right, result, true, ctx, f, body)?,
        "less" | "greater" | "less_equal" | "greater_equal" => {
            cmp_op(left, right, result, name, ctx, f, body)?;
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
    ctx: &mut Ctx,
    f: &mut Function,
    body: &MirBody,
) -> Result<(), String> {
    let left_ty = wasm_scalar_type(body, left).ok_or_else(|| "missing left type".to_owned())?;
    let right_ty = wasm_scalar_type(body, right).ok_or_else(|| "missing right type".to_owned())?;
    if left_ty == WasmScalar::Null || right_ty == WasmScalar::Null {
        local_get(f, ctx.tag_local(left));
        i32(f, TAG_NULL);
        if negate {
            f.instruction(&Instruction::I32Ne);
        } else {
            f.instruction(&Instruction::I32Eq);
        }
        i64_extend(f);
        local_set(f, ctx.data_local(result));
    } else if left_ty != right_ty {
        i32(f, 0);
    } else if left_ty == WasmScalar::Float {
        emit_tag_check_mut(f, ctx, left, TAG_FLOAT);
        emit_tag_check_mut(f, ctx, right, TAG_FLOAT);
        emit_value_as_f64(f, ctx, left, left_ty);
        emit_value_as_f64(f, ctx, right, right_ty);
        f.instruction(&Instruction::F64Eq);
    } else {
        emit_tag_check_mut(f, ctx, left, tag_for_scalar(left_ty)?);
        emit_tag_check_mut(f, ctx, right, tag_for_scalar(right_ty)?);
        local_get(f, ctx.data_local(left));
        local_get(f, ctx.data_local(right));
        f.instruction(&Instruction::I64Eq);
    }
    if negate {
        f.instruction(&Instruction::I32Eqz);
    }
    i64_extend(f);
    local_set(f, ctx.data_local(result));
    record_known_tag(ctx, result, TAG_BOOL);
    Ok(())
}

fn cmp_op(
    left: MirValueId,
    right: MirValueId,
    result: MirValueId,
    op: &str,
    ctx: &mut Ctx,
    f: &mut Function,
    body: &MirBody,
) -> Result<(), String> {
    let left_ty = wasm_scalar_type(body, left).ok_or_else(|| "missing left type".to_owned())?;
    let right_ty = wasm_scalar_type(body, right).ok_or_else(|| "missing right type".to_owned())?;
    if left_ty == WasmScalar::Int && right_ty == WasmScalar::Int {
        emit_tag_check_mut(f, ctx, left, TAG_INT);
        emit_tag_check_mut(f, ctx, right, TAG_INT);
        local_get(f, ctx.data_local(left));
        local_get(f, ctx.data_local(right));
        match op {
            "less" => f.instruction(&Instruction::I64LtS),
            "greater" => f.instruction(&Instruction::I64GtS),
            "less_equal" => f.instruction(&Instruction::I64LeS),
            "greater_equal" => f.instruction(&Instruction::I64GeS),
            _ => f.instruction(&Instruction::Unreachable),
        };
    } else {
        emit_tag_check_mut(f, ctx, left, tag_for_scalar(left_ty)?);
        emit_tag_check_mut(f, ctx, right, tag_for_scalar(right_ty)?);
        emit_value_as_f64(f, ctx, left, left_ty);
        emit_value_as_f64(f, ctx, right, right_ty);
        match op {
            "less" => f.instruction(&Instruction::F64Lt),
            "greater" => f.instruction(&Instruction::F64Gt),
            "less_equal" => f.instruction(&Instruction::F64Le),
            "greater_equal" => f.instruction(&Instruction::F64Ge),
            _ => f.instruction(&Instruction::Unreachable),
        };
    }
    i64_extend(f);
    local_set(f, ctx.data_local(result));
    record_known_tag(ctx, result, TAG_BOOL);
    Ok(())
}

fn emit_numeric_binary(
    name: &str,
    left: MirValueId,
    right: MirValueId,
    result: MirValueId,
    ctx: &mut Ctx,
    f: &mut Function,
    body: &MirBody,
) -> Result<(), String> {
    let left_ty = wasm_scalar_type(body, left).ok_or_else(|| "missing left type".to_owned())?;
    let right_ty = wasm_scalar_type(body, right).ok_or_else(|| "missing right type".to_owned())?;
    if matches!(name, "divide" | "remainder") {
        if left_ty == WasmScalar::Int && right_ty == WasmScalar::Int {
            emit_tag_check(f, ctx, left, TAG_INT);
            emit_tag_check(f, ctx, right, TAG_INT);
            i32(f, TAG_INT);
            local_set(f, ctx.tag_local(result));
            record_known_tag(ctx, result, TAG_INT);
            local_get(f, ctx.data_local(right));
            f.instruction(&Instruction::I64Eqz);
            f.instruction(&Instruction::If(BlockType::Empty));
            f.instruction(&Instruction::Unreachable);
            f.instruction(&Instruction::End);
            local_get(f, ctx.data_local(left));
            local_get(f, ctx.data_local(right));
            if name == "divide" {
                f.instruction(&Instruction::I64DivS);
            } else {
                f.instruction(&Instruction::I64RemS);
            }
            local_set(f, ctx.data_local(result));
            return Ok(());
        }
        return builtin_op_call(
            BuiltinOp::NumericChecked,
            &[left, right],
            &[name.as_bytes().to_vec()],
            result,
            ctx,
            f,
        );
    }
    if left_ty == WasmScalar::Int && right_ty == WasmScalar::Int {
        emit_tag_check_mut(f, ctx, left, TAG_INT);
        emit_tag_check_mut(f, ctx, right, TAG_INT);
        i32(f, TAG_INT);
        local_set(f, ctx.tag_local(result));
        record_known_tag(ctx, result, TAG_INT);
        local_get(f, ctx.data_local(left));
        local_get(f, ctx.data_local(right));
        match name {
            "add" => f.instruction(&Instruction::I64Add),
            "subtract" => f.instruction(&Instruction::I64Sub),
            "multiply" => f.instruction(&Instruction::I64Mul),
            _ => f.instruction(&Instruction::Unreachable),
        };
        local_set(f, ctx.data_local(result));
    } else {
        emit_tag_check(f, ctx, left, tag_for_scalar(left_ty)?);
        emit_tag_check(f, ctx, right, tag_for_scalar(right_ty)?);
        i32(f, TAG_FLOAT);
        local_set(f, ctx.tag_local(result));
        emit_value_as_f64(f, ctx, left, left_ty);
        emit_value_as_f64(f, ctx, right, right_ty);
        match name {
            "add" => f.instruction(&Instruction::F64Add),
            "subtract" => f.instruction(&Instruction::F64Sub),
            "multiply" => f.instruction(&Instruction::F64Mul),
            _ => f.instruction(&Instruction::Unreachable),
        };
        f.instruction(&Instruction::I64ReinterpretF64);
        local_set(f, ctx.data_local(result));
    }
    Ok(())
}

fn emit_value_as_f64(f: &mut Function, ctx: &Ctx, value: MirValueId, ty: WasmScalar) {
    local_get(f, ctx.data_local(value));
    match ty {
        WasmScalar::Int => {
            f.instruction(&Instruction::F64ConvertI64S);
        }
        WasmScalar::Float => {
            f.instruction(&Instruction::F64ReinterpretI64);
        }
        WasmScalar::Bool | WasmScalar::String | WasmScalar::Null => {
            f.instruction(&Instruction::Unreachable);
        }
    }
}

fn emit_tag_check(f: &mut Function, ctx: &Ctx, value: MirValueId, expected: i32) {
    if ctx.known_tags.get(&value) == Some(&expected) {
        return;
    }
    local_get(f, ctx.tag_local(value));
    i32(f, expected);
    f.instruction(&Instruction::I32Ne);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::Unreachable);
    f.instruction(&Instruction::End);
}

fn emit_tag_check_mut(f: &mut Function, ctx: &mut Ctx, value: MirValueId, expected: i32) {
    if ctx.known_tags.get(&value) == Some(&expected) {
        return;
    }
    local_get(f, ctx.tag_local(value));
    i32(f, expected);
    f.instruction(&Instruction::I32Ne);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::Unreachable);
    f.instruction(&Instruction::End);
}

fn record_known_tag(ctx: &mut Ctx, value: MirValueId, tag: i32) {
    ctx.known_tags.insert(value, tag);
}

fn primitive_tag_for_type(ty: &VoxType) -> Option<i32> {
    match ty {
        VoxType::Int => Some(TAG_INT),
        VoxType::Float => Some(TAG_FLOAT),
        VoxType::Bool => Some(TAG_BOOL),
        VoxType::String => Some(TAG_STRING),
        VoxType::OpaqueSurface(name) => match name.as_str() {
            "Int" => Some(TAG_INT),
            "Float" => Some(TAG_FLOAT),
            "Bool" => Some(TAG_BOOL),
            "String" => Some(TAG_STRING),
            "Null" => Some(TAG_NULL),
            _ => None,
        },
        _ => None,
    }
}

fn literal_tag(value: &MirValue) -> Option<i32> {
    match &value.definition {
        MirValueDefinition::Literal => match value.ty.as_ref() {
            Some(VoxType::Int) => Some(TAG_INT),
            Some(VoxType::OpaqueSurface(s)) if s == "Int" => Some(TAG_INT),
            Some(VoxType::Float) => Some(TAG_FLOAT),
            Some(VoxType::OpaqueSurface(s)) if s == "Float" => Some(TAG_FLOAT),
            Some(VoxType::Bool) => Some(TAG_BOOL),
            Some(VoxType::OpaqueSurface(s)) if s == "Bool" => Some(TAG_BOOL),
            _ => None,
        },
        _ => None,
    }
}

fn track_binary_result_tag(
    name: &str,
    left: MirValueId,
    result: MirValueId,
    ctx: &mut Ctx,
    body: &MirBody,
) {
    match name {
        "less" | "greater" | "less_equal" | "greater_equal" | "equal" | "not_equal" => {
            record_known_tag(ctx, result, TAG_BOOL);
        }
        "add" | "subtract" | "multiply" | "divide" | "remainder" => {
            if let Some(WasmScalar::Int) = wasm_scalar_type(body, left) {
                record_known_tag(ctx, result, TAG_INT);
            } else if let Some(WasmScalar::Float) = wasm_scalar_type(body, left) {
                record_known_tag(ctx, result, TAG_FLOAT);
            }
        }
        _ => {}
    }
}

fn track_unary_result_tag(
    name: &str,
    arg: MirValueId,
    result: MirValueId,
    ctx: &mut Ctx,
    body: &MirBody,
) {
    match name {
        "negate" => {
            if let Some(WasmScalar::Int) = wasm_scalar_type(body, arg) {
                record_known_tag(ctx, result, TAG_INT);
            } else if let Some(WasmScalar::Float) = wasm_scalar_type(body, arg) {
                record_known_tag(ctx, result, TAG_FLOAT);
            }
        }
        "not" => {
            record_known_tag(ctx, result, TAG_BOOL);
        }
        _ => {}
    }
}

fn tag_for_scalar(ty: WasmScalar) -> Result<i32, String> {
    match ty {
        WasmScalar::Int => Ok(TAG_INT),
        WasmScalar::Float => Ok(TAG_FLOAT),
        WasmScalar::Bool => Ok(TAG_BOOL),
        WasmScalar::String => Ok(TAG_STRING),
        WasmScalar::Null => Ok(TAG_NULL),
    }
}

fn emit_string_new(
    bytes: &[u8],
    result: MirValueId,
    ctx: &Ctx,
    f: &mut Function,
) -> Result<(), String> {
    let size = 4u32
        .checked_add(bytes.len() as u32)
        .ok_or_else(|| "string allocation size overflow".to_owned())?;
    emit_heap_alloc_const(size, result, ctx, f);
    i32(f, TAG_STRING);
    local_set(f, ctx.tag_local(result));
    emit_i32_store_at_heap_value(f, ctx, result, 0, bytes.len() as i32);
    for (i, byte) in bytes.iter().enumerate() {
        emit_addr_at_heap_value(f, ctx, result, 4 + i as u32);
        i32(f, *byte as i32);
        f.instruction(&Instruction::I32Store8(MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
    }
    Ok(())
}

fn emit_tuple_new(
    args: &[MirValueId],
    result: MirValueId,
    ctx: &Ctx,
    f: &mut Function,
) -> Result<(), String> {
    emit_value_sequence_new(TAG_TUPLE, args, result, ctx, f)
}

fn emit_list_new(
    args: &[MirValueId],
    result: MirValueId,
    ctx: &Ctx,
    f: &mut Function,
) -> Result<(), String> {
    emit_value_sequence_new(TAG_LIST, args, result, ctx, f)
}

fn emit_value_sequence_new(
    tag: i32,
    args: &[MirValueId],
    result: MirValueId,
    ctx: &Ctx,
    f: &mut Function,
) -> Result<(), String> {
    let size = 4u32
        .checked_add(
            (args.len() as u32)
                .checked_mul(16)
                .ok_or_else(|| "sequence allocation size overflow".to_owned())?,
        )
        .ok_or_else(|| "sequence allocation size overflow".to_owned())?;
    emit_heap_alloc_const(size, result, ctx, f);
    i32(f, tag);
    local_set(f, ctx.tag_local(result));
    emit_i32_store_at_heap_value(f, ctx, result, 0, args.len() as i32);
    for (i, arg) in args.iter().enumerate() {
        let base = 4 + i as u32 * 16;
        emit_addr_at_heap_value(f, ctx, result, base);
        local_get(f, ctx.tag_local(*arg));
        f.instruction(&Instruction::I32Store(MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
        emit_addr_at_heap_value(f, ctx, result, base + 8);
        local_get(f, ctx.data_local(*arg));
        f.instruction(&Instruction::I64Store(MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
    }
    Ok(())
}

fn emit_record_new(
    args: &[MirValueId],
    names: &[Vec<u8>],
    result: MirValueId,
    ctx: &Ctx,
    f: &mut Function,
) -> Result<(), String> {
    if args.len() != names.len() {
        return Err(format!(
            "record constructor expected {} names, received {}",
            args.len(),
            names.len()
        ));
    }

    let mut size = 4u32;
    for name in names {
        size = size
            .checked_add(4)
            .and_then(|v| v.checked_add(name.len() as u32))
            .and_then(|v| v.checked_add(12))
            .ok_or_else(|| "record allocation size overflow".to_owned())?;
    }

    emit_heap_alloc_const(size, result, ctx, f);
    i32(f, TAG_RECORD);
    local_set(f, ctx.tag_local(result));
    emit_i32_store_at_heap_value(f, ctx, result, 0, args.len() as i32);

    let mut pos = 4u32;
    for (arg, name) in args.iter().zip(names) {
        emit_i32_store_at_heap_value(f, ctx, result, pos, name.len() as i32);
        pos += 4;
        for byte in name {
            emit_addr_at_heap_value(f, ctx, result, pos);
            i32(f, *byte as i32);
            f.instruction(&Instruction::I32Store8(MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            pos += 1;
        }
        emit_addr_at_heap_value(f, ctx, result, pos);
        local_get(f, ctx.tag_local(*arg));
        f.instruction(&Instruction::I32Store(MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
        pos += 4;
        emit_addr_at_heap_value(f, ctx, result, pos);
        local_get(f, ctx.data_local(*arg));
        f.instruction(&Instruction::I64Store(MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
        pos += 8;
    }
    Ok(())
}

fn emit_heap_alloc_const(size: u32, result: MirValueId, ctx: &Ctx, f: &mut Function) {
    let size = align_to(size, 8);
    f.instruction(&Instruction::GlobalGet(HEAP_TOP_GLOBAL));
    f.instruction(&Instruction::I64ExtendI32U);
    local_set(f, ctx.data_local(result));

    f.instruction(&Instruction::GlobalGet(HEAP_TOP_GLOBAL));
    i32(f, size as i32);
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalTee(ctx.result_tag_local()));
    i32(f, HEAP_LIMIT as i32);
    f.instruction(&Instruction::I32GtU);
    f.instruction(&Instruction::If(BlockType::Empty));
    i32(f, BuiltinOp::HeapExhausted as i32);
    i32(f, SCRATCH_OFF as i32);
    i32(f, 0);
    i32(f, 0);
    i32(f, 0);
    i32(f, RESULT_OFF as i32);
    f.instruction(&Instruction::Call(0));
    f.instruction(&Instruction::Unreachable);
    f.instruction(&Instruction::End);
    local_get(f, ctx.result_tag_local());
    f.instruction(&Instruction::GlobalSet(HEAP_TOP_GLOBAL));
}

fn emit_i32_store_at_heap_value(
    f: &mut Function,
    ctx: &Ctx,
    value: MirValueId,
    offset: u32,
    stored: i32,
) {
    emit_addr_at_heap_value(f, ctx, value, offset);
    i32(f, stored);
    f.instruction(&Instruction::I32Store(MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
}

fn emit_addr_at_heap_value(f: &mut Function, ctx: &Ctx, value: MirValueId, offset: u32) {
    local_get(f, ctx.data_local(value));
    f.instruction(&Instruction::I32WrapI64);
    if offset > 0 {
        i32(f, offset as i32);
        f.instruction(&Instruction::I32Add);
    }
}

fn builtin_op_call(
    op: BuiltinOp,
    args: &[MirValueId],
    extra: &[Vec<u8>],
    result: MirValueId,
    ctx: &mut Ctx,
    f: &mut Function,
) -> Result<(), String> {
    for (i, arg) in args.iter().enumerate() {
        i32(f, (SCRATCH_OFF + i as u32 * 16) as i32);
        local_get(f, ctx.tag_local(*arg));
        f.instruction(&Instruction::I32Store(MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
        i32(f, (SCRATCH_OFF + i as u32 * 16 + 8) as i32);
        local_get(f, ctx.data_local(*arg));
        f.instruction(&Instruction::I64Store(MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
    }

    let extra_scratch = 8192u32;
    let mut pos = extra_scratch;
    for chunk in extra {
        let off = ctx.intern_string(chunk);
        i32(f, pos as i32);
        i32(f, off as i32 + 4);
        f.instruction(&Instruction::I32Store(MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
        i32(f, pos as i32 + 4);
        i32(f, chunk.len() as i32);
        f.instruction(&Instruction::I32Store(MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
        pos += 8;
    }

    i32(f, op as i32);
    i32(f, SCRATCH_OFF as i32);
    i32(f, args.len() as i32);
    i32(
        f,
        if extra.is_empty() {
            0
        } else {
            extra_scratch as i32
        },
    );
    i32(f, extra.len() as i32);
    i32(f, RESULT_OFF as i32);
    f.instruction(&Instruction::Call(0));

    i32(f, RESULT_OFF as i32);
    f.instruction(&Instruction::I32Load(MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    local_set(f, ctx.tag_local(result));
    i32(f, RESULT_OFF as i32 + 8);
    f.instruction(&Instruction::I64Load(MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    local_set(f, ctx.data_local(result));
    Ok(())
}

fn builtin_predicate(
    op: BuiltinOp,
    args: &[MirValueId],
    extra: &[Vec<u8>],
    result: MirValueId,
    ctx: &mut Ctx,
    f: &mut Function,
) -> Result<(), String> {
    builtin_op_call(op, args, extra, result, ctx, f)?;
    i32(f, TAG_BOOL);
    local_set(f, ctx.tag_local(result));
    Ok(())
}

fn emit_vox_call(
    args: &[MirValueId],
    target_func: u32,
    result: MirValueId,
    ctx: &Ctx,
    f: &mut Function,
) -> Result<(), String> {
    for arg in args {
        local_get(f, ctx.tag_local(*arg));
        local_get(f, ctx.data_local(*arg));
    }
    f.instruction(&Instruction::Call(target_func));
    f.instruction(&Instruction::LocalSet(ctx.data_local(result)));
    f.instruction(&Instruction::LocalSet(ctx.tag_local(result)));
    Ok(())
}

fn emit_host_call(
    callee: &str,
    args: &[MirValueId],
    result: MirValueId,
    ctx: &mut Ctx,
    f: &mut Function,
) -> Result<(), String> {
    let callee_bytes = callee.as_bytes().to_vec();
    let callee_offset = ctx.intern_string(&callee_bytes);

    for (i, arg) in args.iter().enumerate() {
        i32(f, (SCRATCH_OFF + i as u32 * 16) as i32);
        local_get(f, ctx.tag_local(*arg));
        f.instruction(&Instruction::I32Store(MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
        i32(f, (SCRATCH_OFF + i as u32 * 16 + 8) as i32);
        local_get(f, ctx.data_local(*arg));
        f.instruction(&Instruction::I64Store(MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
    }

    i32(f, callee_offset as i32 + 4);
    i32(f, callee_bytes.len() as i32);
    i32(f, SCRATCH_OFF as i32);
    i32(f, args.len() as i32);
    i32(f, RESULT_OFF as i32);
    f.instruction(&Instruction::Call(1));

    i32(f, RESULT_OFF as i32);
    f.instruction(&Instruction::I32Load(MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    local_set(f, ctx.tag_local(result));
    i32(f, RESULT_OFF as i32 + 8);
    f.instruction(&Instruction::I64Load(MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    local_set(f, ctx.data_local(result));
    Ok(())
}

fn find_binding_value(body: &MirBody, name: &str) -> Option<MirValueId> {
    let binding = body.bindings.iter().find(|b| b.name == name)?;
    let version_id = binding.versions.last()?;
    let version = body.versions.iter().find(|v| v.id == *version_id)?;
    Some(version.value)
}

fn emit_closure_new(
    body_id: MirBodyId,
    captures: &[MirValueId],
    parameters: &[String],
    result: MirValueId,
    ctx: &mut Ctx,
    f: &mut Function,
) -> Result<(), String> {
    let (table_index, capture_count) = ctx
        .lambda_table
        .get(&body_id)
        .copied()
        .ok_or_else(|| format!("lambda body_id {} not found in lambda_table", body_id.0))?;
    let explicit_params = parameters.len();
    let _type_index = ctx
        .lambda_types
        .get(&explicit_params)
        .copied()
        .ok_or_else(|| format!("no closure type for {} explicit params", explicit_params))?;
    let size = 8u32 + capture_count as u32 * 16;
    emit_heap_alloc_const(size, result, ctx, f);
    i32(f, table_index as i32);
    i32_store_at(f, ctx, result, 0);
    i32(f, capture_count as i32);
    i32_store_at(f, ctx, result, 4);
    for (i, cap) in captures.iter().enumerate().take(capture_count) {
        let offset = 8u32 + i as u32 * 16;
        local_get(f, ctx.tag_local(*cap));
        i32_store_at(f, ctx, result, offset);
        local_get(f, ctx.data_local(*cap));
        i64_store_at(f, ctx, result, offset + 8);
    }
    i32(f, TAG_CLOSURE);
    local_set(f, ctx.tag_local(result));
    Ok(())
}

fn emit_dynamic_call(
    callee_value: MirValueId,
    args: &[MirValueId],
    result: MirValueId,
    ctx: &mut Ctx,
    f: &mut Function,
    _body: &MirBody,
) -> Result<(), String> {
    let closure_data = ctx.data_local(callee_value);

    i32(f, TAG_CLOSURE);
    local_get(f, ctx.tag_local(callee_value));
    f.instruction(&Instruction::I32Ne);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::Unreachable);
    f.instruction(&Instruction::End);

    local_get(f, closure_data);

    for arg in args {
        local_get(f, ctx.tag_local(*arg));
        local_get(f, ctx.data_local(*arg));
    }

    let explicit_params = args.len();
    let type_index = ctx
        .lambda_types
        .get(&explicit_params)
        .copied()
        .unwrap_or_else(|| *ctx.lambda_types.values().next().unwrap_or(&0));

    local_get(f, closure_data);
    f.instruction(&Instruction::I32Load(MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));

    f.instruction(&Instruction::CallIndirect {
        type_index,
        table_index: 0,
    });

    f.instruction(&Instruction::LocalSet(ctx.data_local(result)));
    f.instruction(&Instruction::LocalSet(ctx.tag_local(result)));
    Ok(())
}

fn i32_store_at(f: &mut Function, ctx: &Ctx, value: MirValueId, offset: u32) {
    local_get(f, ctx.data_local(value));
    i32(f, offset as i32);
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::I32Store(MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));
}

fn i64_store_at(f: &mut Function, ctx: &Ctx, value: MirValueId, offset: u32) {
    local_get(f, ctx.data_local(value));
    i32(f, offset as i32);
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::I64Store(MemArg {
        offset: 0,
        align: 3,
        memory_index: 0,
    }));
}

fn emit_lambda_capture_prologue(body: &MirBody, ctx: &Ctx, f: &mut Function) -> Result<(), String> {
    let mut captures: Vec<(MirValueId, u32)> = Vec::new();
    for version in &body.versions {
        if version.source == MirVersionSource::Capture {
            captures.push((version.value, captures.len() as u32));
        }
    }
    if captures.is_empty() {
        return Ok(());
    }

    for (value_id, idx) in &captures {
        let offset = 8u32 + idx * 16;
        i32(f, offset as i32);
        f.instruction(&Instruction::LocalGet(ctx.closure_local()));
        f.instruction(&Instruction::I32Add);
        f.instruction(&Instruction::I32Load(MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }));
        f.instruction(&Instruction::LocalSet(ctx.tag_local(*value_id)));

        i32(f, offset as i32 + 8);
        f.instruction(&Instruction::LocalGet(ctx.closure_local()));
        f.instruction(&Instruction::I32Add);
        f.instruction(&Instruction::I64Load(MemArg {
            offset: 0,
            align: 3,
            memory_index: 0,
        }));
        f.instruction(&Instruction::LocalSet(ctx.data_local(*value_id)));
    }
    Ok(())
}

fn i32(f: &mut Function, v: i32) {
    f.instruction(&Instruction::I32Const(v));
}

fn i64(f: &mut Function, v: i64) {
    f.instruction(&Instruction::I64Const(v));
}

fn local_get(f: &mut Function, idx: u32) {
    f.instruction(&Instruction::LocalGet(idx));
}

fn local_set(f: &mut Function, idx: u32) {
    f.instruction(&Instruction::LocalSet(idx));
}

fn i64_extend(f: &mut Function) {
    f.instruction(&Instruction::I64ExtendI32S);
}

fn align_to(value: u32, align: u32) -> u32 {
    debug_assert!(align.is_power_of_two());
    (value + align - 1) & !(align - 1)
}
