use std::collections::BTreeMap;

use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, DataSection, DataSegment, DataSegmentMode, EntityType,
    ExportKind, ExportSection, Function, FunctionSection, ImportSection, Instruction, MemArg,
    MemoryType, Module, ValType,
};

use vox_core::{
    mir::{
        MirBlock, MirBlockId, MirBody, MirBodyKind, MirModule, MirOpKind, MirPathSegment,
        MirProjection, MirTerminator, MirValueId,
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
    StringBinary = 15,
    NumericChecked = 16,
    RangeNew = 17,
}

struct Ctx {
    value_index: BTreeMap<MirValueId, u32>,
    block_index: BTreeMap<MirBlockId, u32>,
    string_data: Vec<u8>,
    string_offsets: BTreeMap<Vec<u8>, u32>,
    temp_count: u32,
    func_map: BTreeMap<String, u32>,
}

impl Ctx {
    fn new(body: &MirBody, _module: &MirModule) -> Self {
        let mut ctx = Self {
            value_index: BTreeMap::new(),
            block_index: BTreeMap::new(),
            string_data: Vec::new(),
            string_offsets: BTreeMap::new(),
            temp_count: 0,
            func_map: BTreeMap::new(),
        };
        ctx.init_for_body(body);
        ctx
    }

    fn init_for_body(&mut self, body: &MirBody) {
        self.value_index.clear();
        self.block_index.clear();
        self.temp_count = 0;
        let mut idx = 0u32;
        for v in &body.values {
            if !self.value_index.contains_key(&v.id) {
                self.value_index.insert(v.id, idx);
                idx += 1;
            }
        }
        for p in &body.parameters {
            if !self.value_index.contains_key(p) {
                self.value_index.insert(*p, idx);
                idx += 1;
            }
        }
        for (i, b) in body.blocks.iter().enumerate() {
            self.block_index.insert(b.id, i as u32);
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
        self.value_index.get(&vid).copied().unwrap_or(0) * 2 + 1
    }

    fn data_local(&self, vid: MirValueId) -> u32 {
        self.value_index.get(&vid).copied().unwrap_or(0) * 2 + 2
    }

    fn num_value_locals(&self) -> u32 {
        self.value_index.len() as u32 * 2
    }

    fn total_locals(&self) -> u32 {
        self.num_value_locals() + self.temp_count * 2 + 3
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
    let local = idx + 1;
    if local == ctx.block_id_local() || local == ctx.result_tag_local() {
        ValType::I32
    } else if local == ctx.result_data_local() {
        ValType::I64
    } else if local % 2 == 1 {
        ValType::I32
    } else {
        ValType::I64
    }
}

fn lower_module(module: &MirModule) -> Result<Vec<u8>, String> {
    let mut wasm = Module::new();

    let mut types = wasm_encoder::TypeSection::new();
    types.ty().function([ValType::I32; 6], []);
    types.ty().function([ValType::I32; 5], []);
    types
        .ty()
        .function([ValType::I32], [ValType::I32, ValType::I64]);
    wasm.section(&types);

    let mut imports = ImportSection::new();
    imports.import(
        "vox",
        "memory",
        EntityType::Memory(MemoryType {
            minimum: 1,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        }),
    );
    imports.import("vox", "__vox_op", EntityType::Function(0));
    imports.import("vox", "__vox_host", EntityType::Function(1));
    wasm.section(&imports);

    let mut funcs = FunctionSection::new();

    let bodies: Vec<&MirBody> = module
        .bodies
        .iter()
        .filter(|b| !b.blocks.is_empty())
        .collect();

    let mut func_map: BTreeMap<String, u32> = BTreeMap::new();
    let mut func_index = 3u32;
    for body in &bodies {
        func_map.insert(body.name.clone(), func_index);
        funcs.function(2);
        func_index += 1;
    }
    wasm.section(&funcs);

    let mut exports = ExportSection::new();
    exports.export("script_entry", ExportKind::Func, 3);
    exports.export("memory", ExportKind::Memory, 0);
    wasm.section(&exports);

    let mut ctx = Ctx::new(bodies[0], module);
    ctx.func_map = func_map;

    let mut codes = CodeSection::new();
    for (i, body) in bodies.iter().enumerate() {
        if i > 0 {
            ctx.init_for_body(body);
        }
        let func = emit_body(body, &mut ctx)?;
        codes.function(&func);
    }
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
                validate_wasm_op(body, &op.kind, &op.args)?;
            }
            validate_wasm_terminator(body, &block.terminator)?;
        }
    }

    Ok(())
}

fn validate_wasm_op(body: &MirBody, kind: &MirOpKind, args: &[MirValueId]) -> Result<(), String> {
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
        MirOpKind::Call { .. } => Ok(()),
        MirOpKind::Lambda { .. } | MirOpKind::Econ { .. } => Ok(()),
        MirOpKind::Iterator | MirOpKind::IteratorNext => Ok(()),
        MirOpKind::Unknown(_) => Err("unknown MIR op".to_owned()),
    }
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
    matches!(
        ty,
        None | Some(VoxType::Int | VoxType::Float | VoxType::Bool | VoxType::String)
            | Some(VoxType::Tuple(_) | VoxType::Record(_) | VoxType::List(_))
    )
}

fn wasm_scalar_type(body: &MirBody, value: MirValueId) -> Option<WasmScalar> {
    match value_type(body, value) {
        Some(VoxType::Int) => Some(WasmScalar::Int),
        Some(VoxType::Float) => Some(WasmScalar::Float),
        Some(VoxType::Bool) => Some(WasmScalar::Bool),
        Some(VoxType::String) => Some(WasmScalar::String),
        None => Some(WasmScalar::Null),
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

    for (i, param) in body.parameters.iter().enumerate() {
        let tag_loc = ctx.tag_local(*param);
        let data_loc = ctx.data_local(*param);
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::I32Const((4 + i as u32 * 16) as i32));
        f.instruction(&Instruction::I32Add);
        f.instruction(&Instruction::I32Load(MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
        f.instruction(&Instruction::LocalSet(tag_loc));
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::I32Const((8 + i as u32 * 16) as i32));
        f.instruction(&Instruction::I32Add);
        f.instruction(&Instruction::I64Load(MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
        f.instruction(&Instruction::LocalSet(data_loc));
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
                f.instruction(&Instruction::LocalGet(ctx.tag_local(*condition)));
                f.instruction(&Instruction::I32Const(TAG_BOOL));
                f.instruction(&Instruction::I32Ne);
                f.instruction(&Instruction::If(BlockType::Empty));
                f.instruction(&Instruction::Unreachable);
                f.instruction(&Instruction::End);
                f.instruction(&Instruction::LocalGet(ctx.data_local(*condition)));
                f.instruction(&Instruction::I64Const(0));
                f.instruction(&Instruction::I64Ne);
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
                builtin_op_call(BuiltinOp::TupleNew, &[], &[], rid, ctx, f)?;
            }
        }
        MirOpKind::Use(_) | MirOpKind::TypeRefine(_) => {
            if let (Some(rid), Some(&arg)) = (result, args.first()) {
                local_get(f, ctx.tag_local(arg));
                local_set(f, ctx.tag_local(rid));
                local_get(f, ctx.data_local(arg));
                local_set(f, ctx.data_local(rid));
            }
        }
        MirOpKind::Bind(_) | MirOpKind::CacheGet(_) | MirOpKind::CachePut(_) | MirOpKind::Drop => {}
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
                    }
                }
            }
        }
        MirOpKind::Tuple { .. } => {
            if let Some(rid) = result {
                builtin_op_call(BuiltinOp::TupleNew, args, &[], rid, ctx, f)?;
            }
        }
        MirOpKind::Record { fields } => {
            if let Some(rid) = result {
                let names: Vec<Vec<u8>> = fields.iter().map(|n| n.as_bytes().to_vec()).collect();
                builtin_op_call(BuiltinOp::RecordNew, args, &names, rid, ctx, f)?;
            }
        }
        MirOpKind::List => {
            if let Some(rid) = result {
                builtin_op_call(BuiltinOp::ListNew, args, &[], rid, ctx, f)?;
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
                if let Some(&target_func) = ctx.func_map.get(callee) {
                    emit_vox_call(args, target_func, rid, ctx, f)?;
                } else {
                    emit_host_call(callee, args, rid, ctx, f)?;
                }
            }
        }
        MirOpKind::Lambda { parameters } => {
            if let Some(rid) = result {
                let ps: Vec<Vec<u8>> = parameters.iter().map(|p| p.as_bytes().to_vec()).collect();
                builtin_op_call(BuiltinOp::LambdaNew, &[], &ps, rid, ctx, f)?;
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
            let off = ctx.intern_string(&b);
            i32(f, BuiltinOp::StringNew as i32);
            i32(f, SCRATCH_OFF as i32);
            i32(f, 0);
            i32(f, off as i32 + 4);
            i32(f, b.len() as i32);
            i32(f, RESULT_OFF as i32);
            f.instruction(&Instruction::Call(0));
            i32_load(f, RESULT_OFF);
            local_set(f, ctx.tag_local(rid));
            i64_load(f, RESULT_OFF + 8);
            local_set(f, ctx.data_local(rid));
        }
        InlineValue::Tuple(items) => {
            let mut temp_ids = Vec::new();
            for item in items.iter() {
                let tid = ctx.alloc_temp_value();
                emit_literal(item, Some(tid), ctx, f)?;
                temp_ids.push(tid);
            }
            builtin_op_call(BuiltinOp::TupleNew, &temp_ids, &[], rid, ctx, f)?;
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
            builtin_op_call(BuiltinOp::RecordNew, &temp_ids, &names, rid, ctx, f)?;
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
    ctx: &Ctx,
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
        emit_tag_check(f, ctx, left, TAG_FLOAT);
        emit_tag_check(f, ctx, right, TAG_FLOAT);
        emit_value_as_f64(f, ctx, left, left_ty);
        emit_value_as_f64(f, ctx, right, right_ty);
        f.instruction(&Instruction::F64Eq);
    } else {
        emit_tag_check(f, ctx, left, tag_for_scalar(left_ty)?);
        emit_tag_check(f, ctx, right, tag_for_scalar(right_ty)?);
        local_get(f, ctx.data_local(left));
        local_get(f, ctx.data_local(right));
        f.instruction(&Instruction::I64Eq);
    }
    if negate {
        f.instruction(&Instruction::I32Eqz);
    }
    i64_extend(f);
    local_set(f, ctx.data_local(result));
    Ok(())
}

fn cmp_op(
    left: MirValueId,
    right: MirValueId,
    result: MirValueId,
    op: &str,
    ctx: &Ctx,
    f: &mut Function,
    body: &MirBody,
) -> Result<(), String> {
    let left_ty = wasm_scalar_type(body, left).ok_or_else(|| "missing left type".to_owned())?;
    let right_ty = wasm_scalar_type(body, right).ok_or_else(|| "missing right type".to_owned())?;
    if left_ty == WasmScalar::Int && right_ty == WasmScalar::Int {
        emit_tag_check(f, ctx, left, TAG_INT);
        emit_tag_check(f, ctx, right, TAG_INT);
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
        emit_tag_check(f, ctx, left, tag_for_scalar(left_ty)?);
        emit_tag_check(f, ctx, right, tag_for_scalar(right_ty)?);
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
        emit_tag_check(f, ctx, left, TAG_INT);
        emit_tag_check(f, ctx, right, TAG_INT);
        i32(f, TAG_INT);
        local_set(f, ctx.tag_local(result));
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
    local_get(f, ctx.tag_local(value));
    i32(f, expected);
    f.instruction(&Instruction::I32Ne);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::Unreachable);
    f.instruction(&Instruction::End);
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
    for (i, arg) in args.iter().enumerate() {
        i32(f, (SCRATCH_OFF + 4 + i as u32 * 16) as i32);
        local_get(f, ctx.tag_local(*arg));
        f.instruction(&Instruction::I32Store(MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
        i32(f, (SCRATCH_OFF + 8 + i as u32 * 16) as i32);
        local_get(f, ctx.data_local(*arg));
        f.instruction(&Instruction::I64Store(MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
    }
    i32(f, SCRATCH_OFF as i32);
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

fn i32_load(f: &mut Function, offset: u32) {
    i32(f, offset as i32);
    f.instruction(&Instruction::I32Load(MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
}

fn i64_load(f: &mut Function, offset: u32) {
    i32(f, offset as i32);
    f.instruction(&Instruction::I64Load(MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
}

fn i64_extend(f: &mut Function) {
    f.instruction(&Instruction::I64ExtendI32S);
}
