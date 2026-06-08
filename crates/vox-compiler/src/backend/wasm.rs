use std::collections::BTreeMap;

use vox_core::{
    mir::{MirBody, MirBodyKind, MirModule, MirOp, MirOpKind, MirTerminator, MirValueId},
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WasmType {
    I32,
    I64,
    F64,
}

impl WasmBackend {
    pub(crate) fn lower(&self, module: &MirModule) -> WasmLowering {
        let Some(body) = module
            .bodies
            .iter()
            .find(|body| matches!(body.kind, MirBodyKind::ScriptEntry))
        else {
            return WasmLowering::Unsupported("missing script entry body".to_owned());
        };

        match lower_script_entry(body) {
            Ok(bytes) => WasmLowering::Lowered(WasmArtifact {
                bytes,
                entry_export: "script_entry".to_owned(),
                summary: "scalar script-entry wasm".to_owned(),
            }),
            Err(reason) => WasmLowering::Unsupported(reason),
        }
    }
}

fn lower_script_entry(body: &MirBody) -> Result<Vec<u8>, String> {
    if body.blocks.len() != 1 {
        return Err("wasm backend currently lowers only single-block MIR bodies".to_owned());
    }
    let block = body
        .blocks
        .first()
        .ok_or_else(|| "script entry has no MIR block".to_owned())?;

    let value_types = infer_value_types(body)?;
    let params = body
        .parameters
        .iter()
        .map(|value| {
            value_types
                .get(value)
                .copied()
                .ok_or_else(|| format!("parameter %{} has no wasm type", value.0))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let result_type = match &block.terminator {
        MirTerminator::Return(value) => value_types.get(value).copied(),
        MirTerminator::Panic(_)
        | MirTerminator::Unreachable
        | MirTerminator::Jump { .. }
        | MirTerminator::Branch { .. } => None,
    };

    let mut locals = LocalLayout::new();
    for op in &block.ops {
        if let Some(result) = op.result {
            if body.parameters.contains(&result) {
                continue;
            }
            let ty = value_types
                .get(&result)
                .copied()
                .ok_or_else(|| format!("value %{} has no wasm type", result.0))?;
            locals.add(result, ty);
        }
    }

    let mut code = Vec::new();
    let mut local_indices = BTreeMap::new();
    for (index, value) in body.parameters.iter().enumerate() {
        local_indices.insert(*value, index as u32);
    }
    let next_index = body.parameters.len() as u32;
    for (offset, value) in locals.values.iter().enumerate() {
        local_indices.insert(value.id, next_index + offset as u32);
    }

    locals.encode_declarations(&mut code);
    for op in &block.ops {
        emit_op(op, &value_types, &local_indices, &mut code)?;
    }

    match &block.terminator {
        MirTerminator::Return(value) => {
            if result_type.is_some() {
                emit_local_get(*value, &local_indices, &mut code)?;
            }
        }
        MirTerminator::Panic(_) | MirTerminator::Unreachable => code.push(0x00),
        MirTerminator::Jump { .. } | MirTerminator::Branch { .. } => {
            return Err("control-flow terminators are not wasm-lowered yet".to_owned());
        }
    }
    code.push(0x0b);

    let mut module = Vec::new();
    module.extend_from_slice(b"\0asm");
    module.extend_from_slice(&1_u32.to_le_bytes());
    write_type_section(&mut module, &params, result_type);
    write_function_section(&mut module);
    write_export_section(&mut module);
    write_code_section(&mut module, &code);
    Ok(module)
}

fn infer_value_types(body: &MirBody) -> Result<BTreeMap<MirValueId, WasmType>, String> {
    let mut types = BTreeMap::new();
    for value in &body.values {
        if let Some(ty) = value.ty.as_ref().and_then(wasm_type_from_vox) {
            types.insert(value.id, ty);
        }
    }

    let block = body
        .blocks
        .first()
        .ok_or_else(|| "script entry has no MIR block".to_owned())?;
    for op in &block.ops {
        let Some(result) = op.result else {
            continue;
        };
        let ty = match &op.kind {
            MirOpKind::Literal(value) => wasm_type_from_literal(value),
            MirOpKind::Unit => None,
            MirOpKind::Use(_) | MirOpKind::TypeRefine(_) => {
                op.args.first().and_then(|arg| types.get(arg)).copied()
            }
            MirOpKind::Unary(name) => {
                let arg = op
                    .args
                    .first()
                    .and_then(|arg| types.get(arg))
                    .copied()
                    .ok_or_else(|| format!("unary op result %{} has untyped operand", result.0))?;
                match name.as_str() {
                    "not" => Some(WasmType::I32),
                    "negate" => Some(arg),
                    _ => return Err(format!("unsupported unary wasm op `{name}`")),
                }
            }
            MirOpKind::Binary(name) => match name.as_str() {
                "less" | "less_equal" | "greater" | "greater_equal" | "equal" | "not_equal" => {
                    Some(WasmType::I32)
                }
                _ => op.args.first().and_then(|arg| types.get(arg)).copied(),
            },
            MirOpKind::TypeTest(_) => Some(WasmType::I32),
            MirOpKind::Bind(_)
            | MirOpKind::Tuple { .. }
            | MirOpKind::Record { .. }
            | MirOpKind::List
            | MirOpKind::StringInterpolate { .. }
            | MirOpKind::Project(_)
            | MirOpKind::Index
            | MirOpKind::Updated { .. }
            | MirOpKind::Call { .. }
            | MirOpKind::Econ { .. }
            | MirOpKind::NonNull
            | MirOpKind::SafeProject(_)
            | MirOpKind::Iterator
            | MirOpKind::IteratorNext
            | MirOpKind::CacheGet(_)
            | MirOpKind::CachePut(_)
            | MirOpKind::Drop
            | MirOpKind::Unknown(_) => None,
        };

        if let Some(ty) = ty {
            types.insert(result, ty);
        }
    }

    Ok(types)
}

fn emit_op(
    op: &MirOp,
    value_types: &BTreeMap<MirValueId, WasmType>,
    local_indices: &BTreeMap<MirValueId, u32>,
    code: &mut Vec<u8>,
) -> Result<(), String> {
    match &op.kind {
        MirOpKind::Literal(value) => {
            emit_literal(value, code)?;
            emit_result_set(op, local_indices, code)?;
        }
        MirOpKind::Use(_) | MirOpKind::TypeRefine(_) => {
            let source = one_arg(op)?;
            emit_local_get(source, local_indices, code)?;
            emit_result_set(op, local_indices, code)?;
        }
        MirOpKind::Bind(_) | MirOpKind::CachePut(_) | MirOpKind::Drop => {}
        MirOpKind::Unary(name) => {
            let source = one_arg(op)?;
            let source_ty = value_types
                .get(&source)
                .copied()
                .ok_or_else(|| format!("value %{} has no wasm type", source.0))?;
            emit_unary(name, source, source_ty, local_indices, code)?;
            emit_result_set(op, local_indices, code)?;
        }
        MirOpKind::Binary(name) => {
            let (left, right) = two_args(op)?;
            let ty = value_types
                .get(&left)
                .copied()
                .ok_or_else(|| format!("value %{} has no wasm type", left.0))?;
            emit_local_get(left, local_indices, code)?;
            emit_local_get(right, local_indices, code)?;
            emit_binary(name, ty, code)?;
            emit_result_set(op, local_indices, code)?;
        }
        MirOpKind::Unit
        | MirOpKind::Tuple { .. }
        | MirOpKind::Record { .. }
        | MirOpKind::List
        | MirOpKind::StringInterpolate { .. }
        | MirOpKind::Project(_)
        | MirOpKind::Index
        | MirOpKind::Updated { .. }
        | MirOpKind::Call { .. }
        | MirOpKind::Econ { .. }
        | MirOpKind::NonNull
        | MirOpKind::SafeProject(_)
        | MirOpKind::TypeTest(_)
        | MirOpKind::Iterator
        | MirOpKind::IteratorNext
        | MirOpKind::CacheGet(_)
        | MirOpKind::Unknown(_) => {
            return Err(format!("unsupported wasm op `{}`", op_kind_name(&op.kind)));
        }
    }
    Ok(())
}

fn emit_unary(
    name: &str,
    source: MirValueId,
    ty: WasmType,
    local_indices: &BTreeMap<MirValueId, u32>,
    code: &mut Vec<u8>,
) -> Result<(), String> {
    match (name, ty) {
        ("not", WasmType::I32) => {
            emit_local_get(source, local_indices, code)?;
            code.push(0x45);
        }
        ("negate", WasmType::I64) => {
            code.push(0x42);
            write_sleb_i64(code, 0);
            emit_local_get(source, local_indices, code)?;
            code.push(0x7d);
        }
        ("negate", WasmType::F64) => {
            emit_local_get(source, local_indices, code)?;
            code.push(0x9a);
        }
        _ => return Err(format!("unsupported unary wasm op `{name}`")),
    }
    Ok(())
}

fn emit_binary(name: &str, ty: WasmType, code: &mut Vec<u8>) -> Result<(), String> {
    let opcode = match (name, ty) {
        ("add", WasmType::I64) => 0x7c,
        ("subtract", WasmType::I64) => 0x7d,
        ("multiply", WasmType::I64) => 0x7e,
        ("divide", WasmType::I64) => 0x7f,
        ("remainder", WasmType::I64) => 0x81,
        ("equal", WasmType::I64) => 0x51,
        ("not_equal", WasmType::I64) => 0x52,
        ("less", WasmType::I64) => 0x53,
        ("greater", WasmType::I64) => 0x55,
        ("less_equal", WasmType::I64) => 0x57,
        ("greater_equal", WasmType::I64) => 0x59,
        ("add", WasmType::F64) => 0xa0,
        ("subtract", WasmType::F64) => 0xa1,
        ("multiply", WasmType::F64) => 0xa2,
        ("divide", WasmType::F64) => 0xa3,
        ("equal", WasmType::F64) => 0x61,
        ("not_equal", WasmType::F64) => 0x62,
        ("less", WasmType::F64) => 0x63,
        ("greater", WasmType::F64) => 0x64,
        ("less_equal", WasmType::F64) => 0x65,
        ("greater_equal", WasmType::F64) => 0x66,
        ("add", WasmType::I32) => 0x6a,
        ("subtract", WasmType::I32) => 0x6b,
        ("multiply", WasmType::I32) => 0x6c,
        ("divide", WasmType::I32) => 0x6d,
        ("remainder", WasmType::I32) => 0x6f,
        ("equal", WasmType::I32) => 0x46,
        ("not_equal", WasmType::I32) => 0x47,
        ("less", WasmType::I32) => 0x48,
        ("greater", WasmType::I32) => 0x4a,
        ("less_equal", WasmType::I32) => 0x4c,
        ("greater_equal", WasmType::I32) => 0x4e,
        _ => return Err(format!("unsupported binary wasm op `{name}` for {ty:?}")),
    };
    code.push(opcode);
    Ok(())
}

fn emit_literal(value: &InlineValue, code: &mut Vec<u8>) -> Result<(), String> {
    match value {
        InlineValue::Int(value) => {
            code.push(0x42);
            write_sleb_i64(code, *value);
        }
        InlineValue::Float(value) => {
            code.push(0x44);
            code.extend_from_slice(&value.to_le_bytes());
        }
        InlineValue::Bool(value) => {
            code.push(0x41);
            write_sleb_i32(code, i32::from(*value));
        }
        InlineValue::String(_)
        | InlineValue::Tuple(_)
        | InlineValue::Record(_)
        | InlineValue::Null => {
            return Err("only Int, Float, and Bool literals lower to scalar wasm".to_owned());
        }
    }
    Ok(())
}

fn emit_local_get(
    value: MirValueId,
    local_indices: &BTreeMap<MirValueId, u32>,
    code: &mut Vec<u8>,
) -> Result<(), String> {
    let index = local_indices
        .get(&value)
        .copied()
        .ok_or_else(|| format!("value %{} has no wasm local", value.0))?;
    code.push(0x20);
    write_uleb_u32(code, index);
    Ok(())
}

fn emit_result_set(
    op: &MirOp,
    local_indices: &BTreeMap<MirValueId, u32>,
    code: &mut Vec<u8>,
) -> Result<(), String> {
    if let Some(result) = op.result {
        let index = local_indices
            .get(&result)
            .copied()
            .ok_or_else(|| format!("value %{} has no wasm local", result.0))?;
        code.push(0x21);
        write_uleb_u32(code, index);
    }
    Ok(())
}

fn write_type_section(module: &mut Vec<u8>, params: &[WasmType], result: Option<WasmType>) {
    let mut payload = Vec::new();
    write_uleb_u32(&mut payload, 1);
    payload.push(0x60);
    write_uleb_u32(&mut payload, params.len() as u32);
    for param in params {
        payload.push(param.byte());
    }
    match result {
        Some(result) => {
            write_uleb_u32(&mut payload, 1);
            payload.push(result.byte());
        }
        None => write_uleb_u32(&mut payload, 0),
    }
    write_section(module, 1, &payload);
}

fn write_function_section(module: &mut Vec<u8>) {
    let mut payload = Vec::new();
    write_uleb_u32(&mut payload, 1);
    write_uleb_u32(&mut payload, 0);
    write_section(module, 3, &payload);
}

fn write_export_section(module: &mut Vec<u8>) {
    let mut payload = Vec::new();
    write_uleb_u32(&mut payload, 1);
    write_name(&mut payload, "script_entry");
    payload.push(0x00);
    write_uleb_u32(&mut payload, 0);
    write_section(module, 7, &payload);
}

fn write_code_section(module: &mut Vec<u8>, body: &[u8]) {
    let mut payload = Vec::new();
    write_uleb_u32(&mut payload, 1);
    write_uleb_u32(&mut payload, body.len() as u32);
    payload.extend_from_slice(body);
    write_section(module, 10, &payload);
}

fn write_section(module: &mut Vec<u8>, id: u8, payload: &[u8]) {
    module.push(id);
    write_uleb_u32(module, payload.len() as u32);
    module.extend_from_slice(payload);
}

fn write_name(out: &mut Vec<u8>, name: &str) {
    write_uleb_u32(out, name.len() as u32);
    out.extend_from_slice(name.as_bytes());
}

fn one_arg(op: &MirOp) -> Result<MirValueId, String> {
    match op.args.as_slice() {
        [value] => Ok(*value),
        _ => Err(format!(
            "wasm op `{}` expects one operand",
            op_kind_name(&op.kind)
        )),
    }
}

fn two_args(op: &MirOp) -> Result<(MirValueId, MirValueId), String> {
    match op.args.as_slice() {
        [left, right] => Ok((*left, *right)),
        _ => Err(format!(
            "wasm op `{}` expects two operands",
            op_kind_name(&op.kind)
        )),
    }
}

#[derive(Debug, Clone, Copy)]
struct LocalValue {
    id: MirValueId,
    ty: WasmType,
}

#[derive(Debug, Default)]
struct LocalLayout {
    values: Vec<LocalValue>,
}

impl LocalLayout {
    fn new() -> Self {
        Self { values: Vec::new() }
    }

    fn add(&mut self, id: MirValueId, ty: WasmType) {
        self.values.push(LocalValue { id, ty });
    }

    fn encode_declarations(&self, code: &mut Vec<u8>) {
        let mut groups = Vec::<(u32, WasmType)>::new();
        for local in &self.values {
            if let Some((count, ty)) = groups.last_mut() {
                if *ty == local.ty {
                    *count += 1;
                    continue;
                }
            }
            groups.push((1, local.ty));
        }

        write_uleb_u32(code, groups.len() as u32);
        for (count, ty) in groups {
            write_uleb_u32(code, count);
            code.push(ty.byte());
        }
    }
}

impl WasmType {
    fn byte(self) -> u8 {
        match self {
            Self::I32 => 0x7f,
            Self::I64 => 0x7e,
            Self::F64 => 0x7c,
        }
    }
}

fn wasm_type_from_vox(ty: &VoxType) -> Option<WasmType> {
    match ty {
        VoxType::Int => Some(WasmType::I64),
        VoxType::Float => Some(WasmType::F64),
        VoxType::Bool => Some(WasmType::I32),
        VoxType::OpaqueSurface(raw) => match raw.as_str() {
            "Int" => Some(WasmType::I64),
            "Float" => Some(WasmType::F64),
            "Bool" => Some(WasmType::I32),
            _ => None,
        },
        _ => None,
    }
}

fn wasm_type_from_literal(value: &InlineValue) -> Option<WasmType> {
    match value {
        InlineValue::Int(_) => Some(WasmType::I64),
        InlineValue::Float(_) => Some(WasmType::F64),
        InlineValue::Bool(_) => Some(WasmType::I32),
        InlineValue::String(_)
        | InlineValue::Tuple(_)
        | InlineValue::Record(_)
        | InlineValue::Null => None,
    }
}

fn op_kind_name(kind: &MirOpKind) -> &'static str {
    match kind {
        MirOpKind::Literal(_) => "literal",
        MirOpKind::Unit => "unit",
        MirOpKind::Use(_) => "use",
        MirOpKind::Bind(_) => "bind",
        MirOpKind::Unary(_) => "unary",
        MirOpKind::Binary(_) => "binary",
        MirOpKind::Tuple { .. } => "tuple",
        MirOpKind::Record { .. } => "record",
        MirOpKind::List => "list",
        MirOpKind::StringInterpolate { .. } => "string_interpolate",
        MirOpKind::Project(_) => "project",
        MirOpKind::Index => "index",
        MirOpKind::Updated { .. } => "updated",
        MirOpKind::Call { .. } => "call",
        MirOpKind::Econ { .. } => "econ",
        MirOpKind::NonNull => "non_null",
        MirOpKind::SafeProject(_) => "safe_project",
        MirOpKind::TypeTest(_) => "type_test",
        MirOpKind::TypeRefine(_) => "type_refine",
        MirOpKind::Iterator => "iterator",
        MirOpKind::IteratorNext => "iterator_next",
        MirOpKind::CacheGet(_) => "cache_get",
        MirOpKind::CachePut(_) => "cache_put",
        MirOpKind::Drop => "drop",
        MirOpKind::Unknown(_) => "unknown",
    }
}

fn write_uleb_u32(out: &mut Vec<u8>, mut value: u32) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn write_sleb_i32(out: &mut Vec<u8>, mut value: i32) {
    loop {
        let byte = (value as u8) & 0x7f;
        value >>= 7;
        let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
        out.push(if done { byte } else { byte | 0x80 });
        if done {
            break;
        }
    }
}

fn write_sleb_i64(out: &mut Vec<u8>, mut value: i64) {
    loop {
        let byte = (value as u8) & 0x7f;
        value >>= 7;
        let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
        out.push(if done { byte } else { byte | 0x80 });
        if done {
            break;
        }
    }
}
