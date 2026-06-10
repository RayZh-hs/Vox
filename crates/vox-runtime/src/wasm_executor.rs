use wasmtime::*;

use vox_core::{
    ids::HandleId,
    source::ModulePath,
    value::{HandleSummary, InlineValue, RuntimeValue},
};

use crate::{HostCallArgument, Runtime};

const TAG_INT: i32 = 0;
const TAG_FLOAT: i32 = 1;
const TAG_BOOL: i32 = 2;
const TAG_STRING: i32 = 3;
const TAG_TUPLE: i32 = 4;
const TAG_RECORD: i32 = 5;
const TAG_LIST: i32 = 6;
const TAG_HANDLE: i32 = 7;
const TAG_NULL: i32 = 8;

pub fn try_wasm_execute(
    runtime: &mut Runtime,
    wasm_bytes: &[u8],
    arguments: &[RuntimeValue],
) -> Result<RuntimeValue, String> {
    let engine = Engine::default();
    let module = Module::new(&engine, wasm_bytes).map_err(|e| e.to_string())?;

    let runtime_ptr = runtime as *mut Runtime;

    #[derive(Debug)]
    struct State {
        runtime: *mut Runtime,
    }

    let mut store = Store::new(&engine, State {
        runtime: runtime_ptr,
    });

    let memory_ty = MemoryType::new(1, None);
    let memory = Memory::new(&mut store, memory_ty).map_err(|e| e.to_string())?;

    let vox_op_ty = FuncType::new(&engine, vec![ValType::I32; 6], vec![]);
    let vox_op = Func::new(
        &mut store,
        vox_op_ty.clone(),
        move |mut caller: Caller<'_, State>, params: &[Val], _results: &mut [Val]| {
            let op_id = params[0].unwrap_i32();
            let args_ptr = params[1].unwrap_i32() as u32;
            let arg_count = params[2].unwrap_i32() as u32;
            let extra_ptr = params[3].unwrap_i32() as u32;
            let extra_count = params[4].unwrap_i32() as u32;
            let result_ptr = params[5].unwrap_i32() as u32;

            let state = caller.data();
            let runtime = unsafe { &mut *state.runtime };

            if let Some(mem) = caller.get_export("memory").and_then(|e| e.into_memory()) {
                let data = mem.data(&caller);
                let result = handle_builtin_op(runtime, op_id, args_ptr, arg_count, extra_ptr, extra_count, data);
                if let Ok((tag, val)) = result {
                    let _ = mem.write(&mut caller, result_ptr as usize, &tag.to_le_bytes());
                    let _ = mem.write(&mut caller, result_ptr as usize + 8, &val.to_le_bytes());
                }
            }
            Ok(())
        },
    );

    let vox_host_ty = FuncType::new(&engine, vec![ValType::I32; 5], vec![]);
    let vox_host = Func::new(
        &mut store,
        vox_host_ty,
        move |mut caller: Caller<'_, State>, params: &[Val], _results: &mut [Val]| {
            let callee_ptr = params[0].unwrap_i32() as u32;
            let callee_len = params[1].unwrap_i32() as u32;
            let args_ptr = params[2].unwrap_i32() as u32;
            let arg_count = params[3].unwrap_i32() as u32;
            let result_ptr = params[4].unwrap_i32() as u32;

            let state = caller.data();
            let runtime = unsafe { &mut *state.runtime };

            if let Some(mem) = caller.get_export("memory").and_then(|e| e.into_memory()) {
                let data = mem.data(&caller);
                let result = handle_host_call(runtime, callee_ptr, callee_len, args_ptr, arg_count, data);
                if let Ok((tag, val)) = result {
                    let _ = mem.write(&mut caller, result_ptr as usize, &tag.to_le_bytes());
                    let _ = mem.write(&mut caller, result_ptr as usize + 8, &val.to_le_bytes());
                }
            }
            Ok(())
        },
    );

    let instance = Instance::new(
        &mut store,
        &module,
        &[memory.into(), vox_op.into(), vox_host.into()],
    )
    .map_err(|e| e.to_string())?;

    let mem = instance
        .get_memory(&mut store, "memory")
        .ok_or_else(|| "memory export not found".to_owned())?;

    let mut scratch: u32 = 0;
    for arg in arguments {
        let (tag, val) = to_wasm(arg);
        mem.write(&mut store, scratch as usize + 4, &tag.to_le_bytes())
            .map_err(|e| e.to_string())?;
        mem.write(&mut store, scratch as usize + 8, &val.to_le_bytes())
            .map_err(|e| e.to_string())?;
        scratch += 16;
    }

    let entry = instance
        .get_typed_func::<i32, (i32, i64)>(&mut store, "script_entry")
        .map_err(|e| e.to_string())?;

    let (result_tag, result_data) = entry
        .call(&mut store, 0i32)
        .map_err(|e| e.to_string())?;

    from_wasm(result_tag, result_data)
}

fn handle_builtin_op(
    runtime: &mut Runtime,
    op_id: i32,
    args_ptr: u32,
    arg_count: u32,
    extra_ptr: u32,
    extra_count: u32,
    data: &[u8],
) -> Result<(i32, i64), String> {
    let mut args = Vec::new();
    for i in 0..arg_count {
        let ptr = args_ptr + i * 16;
        let tag = mem_read_i32(data, ptr)?;
        let val = mem_read_i64(data, ptr + 8)?;
        args.push((tag, val));
    }

    let mut extra: Vec<Vec<u8>> = Vec::new();
    for i in 0..extra_count {
        let ptr = extra_ptr + i * 8;
        let off = mem_read_i32(data, ptr)? as u32;
        let len = mem_read_i32(data, ptr + 4)? as u32;
        if let Some(s) = data.get(off as usize..off as usize + len as usize) {
            extra.push(s.to_vec());
        }
    }

    match op_id {
        0 => builtin_tuple_new(runtime, &args),
        1 => builtin_record_new(runtime, &args),
        2 => builtin_list_new(runtime, &args),
        3 => builtin_string_new(runtime, &extra),
        4 => builtin_string_interpolate(runtime, &args, &extra),
        5 => builtin_project(&args, &extra),
        6 => builtin_index(&args),
        7 => builtin_updated(&args, &extra),
        8 => builtin_type_test(&args, &extra),
        9 => builtin_iterator(runtime),
        10 => builtin_iterator_next(runtime),
        11 => builtin_lambda_new(runtime, &extra),
        12 => builtin_econ_new(runtime, &args),
        13 => builtin_non_null(&args),
        14 => builtin_safe_project(&args, &extra),
        _ => Err(format!("unknown builtin op {op_id}")),
    }
}

fn handle_host_call(
    runtime: &mut Runtime,
    callee_ptr: u32,
    callee_len: u32,
    args_ptr: u32,
    arg_count: u32,
    data: &[u8],
) -> Result<(i32, i64), String> {
    let callee_bytes = data
        .get(callee_ptr as usize..callee_ptr as usize + callee_len as usize)
        .ok_or("callee name out of bounds")?;
    let callee = std::str::from_utf8(callee_bytes)
        .map_err(|_| "invalid callee name")?
        .to_owned();

    let mut arg_values = Vec::new();
    for i in 0..arg_count {
        let ptr = args_ptr + i * 16;
        let tag = mem_read_i32(data, ptr)?;
        let val = mem_read_i64(data, ptr + 8)?;
        arg_values.push(from_wasm(tag, val).unwrap_or(RuntimeValue::Inline(InlineValue::Null)));
    }

    let host_args: Vec<HostCallArgument> = arg_values
        .into_iter()
        .enumerate()
        .map(|(i, v)| HostCallArgument {
            name: format!("arg{i}"),
            value: Some(v),
        })
        .collect();

    if let Some((package, function)) = callee.rsplit_once('.') {
        let pkg = ModulePath::parse(package).map_err(|e| format!("bad package: {}", e.message))?;
        let result = runtime.invoke_host_function(&pkg, function, &host_args)?;
        return Ok(to_wasm(&result));
    }

    Err(format!("host call target not found: {callee}"))
}

fn builtin_tuple_new(runtime: &mut Runtime, args: &[(i32, i64)]) -> Result<(i32, i64), String> {
    let items: Vec<InlineValue> = args.iter().map(|(t, v)| wasm_to_inline(*t, *v)).collect();
    let summary = HandleSummary {
        type_name: "Tuple".to_owned(),
        summary: format_args("Tuple", &items),
        bytes: None,
    };
    let handle = runtime.allocate_handle(summary);
    Ok((TAG_TUPLE, handle.0 as i64))
}

fn builtin_record_new(runtime: &mut Runtime, args: &[(i32, i64)]) -> Result<(i32, i64), String> {
    let items: Vec<InlineValue> = args.iter().map(|(t, v)| wasm_to_inline(*t, *v)).collect();
    let summary = HandleSummary {
        type_name: "Record".to_owned(),
        summary: format_args("Record", &items),
        bytes: None,
    };
    let handle = runtime.allocate_handle(summary);
    Ok((TAG_RECORD, handle.0 as i64))
}

fn builtin_list_new(runtime: &mut Runtime, args: &[(i32, i64)]) -> Result<(i32, i64), String> {
    let items: Vec<InlineValue> = args.iter().map(|(t, v)| wasm_to_inline(*t, *v)).collect();
    let summary = HandleSummary {
        type_name: "List".to_owned(),
        summary: format_args("List", &items),
        bytes: None,
    };
    let handle = runtime.allocate_handle(summary);
    Ok((TAG_LIST, handle.0 as i64))
}

fn builtin_string_new(runtime: &mut Runtime, extra: &[Vec<u8>]) -> Result<(i32, i64), String> {
    if extra.is_empty() {
        return Err("StringNew missing data".to_owned());
    }
    let s = String::from_utf8(extra[0].clone()).map_err(|e| format!("invalid utf8: {e}"))?;
    let summary = HandleSummary {
        type_name: "String".to_owned(),
        summary: s.clone(),
        bytes: Some(s.len() as u64),
    };
    let handle = runtime.allocate_handle(summary);
    Ok((TAG_STRING, handle.0 as i64))
}

fn builtin_string_interpolate(
    runtime: &mut Runtime,
    args: &[(i32, i64)],
    segments: &[Vec<u8>],
) -> Result<(i32, i64), String> {
    let text: Vec<String> = segments
        .iter()
        .map(|s| String::from_utf8(s.clone()).unwrap_or_default())
        .collect();
    let mut out = String::new();
    for (i, (tag, val)) in args.iter().enumerate() {
        if i < text.len() {
            out.push_str(&text[i]);
        }
        let v = wasm_to_inline(*tag, *val);
        out.push_str(&render_inline(&v));
    }
    if text.len() > args.len() {
        out.push_str(&text[args.len()]);
    }
    let summary = HandleSummary {
        type_name: "String".to_owned(),
        summary: out.clone(),
        bytes: Some(out.len() as u64),
    };
    let handle = runtime.allocate_handle(summary);
    Ok((TAG_STRING, handle.0 as i64))
}

fn builtin_project(args: &[(i32, i64)], extra: &[Vec<u8>]) -> Result<(i32, i64), String> {
    if args.is_empty() || extra.is_empty() {
        return Err("Project missing args".to_owned());
    }
    let proj_data = &extra[0];
    let _kind = *proj_data.first().unwrap_or(&0);
    Ok((TAG_NULL, 0))
}

fn builtin_index(_args: &[(i32, i64)]) -> Result<(i32, i64), String> {
    Ok((TAG_NULL, 0))
}

fn builtin_updated(_args: &[(i32, i64)], _extra: &[Vec<u8>]) -> Result<(i32, i64), String> {
    Err("updated not implemented".to_owned())
}

fn builtin_type_test(args: &[(i32, i64)], extra: &[Vec<u8>]) -> Result<(i32, i64), String> {
    if extra.is_empty() || args.is_empty() {
        return Err("TypeTest missing data".to_owned());
    }
    let ty = String::from_utf8(extra[0].clone()).unwrap_or_default();
    let expected = name_to_tag(&ty);
    let result = if args[0].0 == expected { 1i64 } else { 0i64 };
    Ok((TAG_BOOL, result))
}

fn builtin_iterator(_runtime: &mut Runtime) -> Result<(i32, i64), String> {
    Err("iterator not implemented".to_owned())
}

fn builtin_iterator_next(_runtime: &mut Runtime) -> Result<(i32, i64), String> {
    Err("iterator_next not implemented".to_owned())
}

fn builtin_lambda_new(runtime: &mut Runtime, extra: &[Vec<u8>]) -> Result<(i32, i64), String> {
    let params: Vec<String> = extra.iter().map(|s| String::from_utf8_lossy(s).to_string()).collect();
    let sig = if params.is_empty() { "()".to_owned() } else { params.join(", ") };
    let summary = HandleSummary {
        type_name: "Function".to_owned(),
        summary: format!("<function <lambda> ({sig})>"),
        bytes: None,
    };
    let handle = runtime.allocate_handle(summary);
    Ok((TAG_HANDLE, handle.0 as i64))
}

fn builtin_econ_new(runtime: &mut Runtime, args: &[(i32, i64)]) -> Result<(i32, i64), String> {
    if args.is_empty() {
        return Err("Econ missing arg".to_owned());
    }
    let v = wasm_to_inline(args[0].0, args[0].1);
    let summary = HandleSummary {
        type_name: "Econ".to_owned(),
        summary: format!("econ({})", render_inline(&v)),
        bytes: None,
    };
    let handle = runtime.allocate_handle(summary);
    Ok((TAG_HANDLE, handle.0 as i64))
}

fn builtin_non_null(args: &[(i32, i64)]) -> Result<(i32, i64), String> {
    if args.is_empty() {
        return Err("NonNull missing arg".to_owned());
    }
    if args[0].0 == TAG_NULL {
        return Err("cannot unwrap null value".to_owned());
    }
    Ok(args[0])
}

fn builtin_safe_project(args: &[(i32, i64)], extra: &[Vec<u8>]) -> Result<(i32, i64), String> {
    if args.is_empty() {
        return Err("SafeProject missing arg".to_owned());
    }
    if args[0].0 == TAG_NULL {
        return Ok((TAG_NULL, 0));
    }
    builtin_project(args, extra)
}

fn to_wasm(value: &RuntimeValue) -> (i32, i64) {
    match value {
        RuntimeValue::Inline(iv) => inline_to_wasm(iv),
        RuntimeValue::Handle(h) => (TAG_HANDLE, h.0 as i64),
    }
}

fn inline_to_wasm(value: &InlineValue) -> (i32, i64) {
    match value {
        InlineValue::Int(v) => (TAG_INT, *v),
        InlineValue::Float(v) => (TAG_FLOAT, v.to_bits() as i64),
        InlineValue::Bool(v) => (TAG_BOOL, *v as i64),
        InlineValue::String(_) => (TAG_STRING, 0),
        InlineValue::Tuple(_) => (TAG_TUPLE, 0),
        InlineValue::Record(_) => (TAG_RECORD, 0),
        InlineValue::Handle(h) => (TAG_HANDLE, h.0 as i64),
        InlineValue::Null => (TAG_NULL, 0),
    }
}

fn from_wasm(tag: i32, val: i64) -> Result<RuntimeValue, String> {
    match tag {
        TAG_INT => Ok(RuntimeValue::Inline(InlineValue::Int(val))),
        TAG_FLOAT => Ok(RuntimeValue::Inline(InlineValue::Float(f64::from_bits(val as u64)))),
        TAG_BOOL => Ok(RuntimeValue::Inline(InlineValue::Bool(val != 0))),
        TAG_NULL => Ok(RuntimeValue::Inline(InlineValue::Null)),
        TAG_HANDLE | TAG_STRING | TAG_TUPLE | TAG_RECORD | TAG_LIST => {
            Ok(RuntimeValue::Handle(HandleId(val as u64)))
        }
        _ => Ok(RuntimeValue::Inline(InlineValue::Null)),
    }
}

fn wasm_to_inline(tag: i32, val: i64) -> InlineValue {
    match tag {
        TAG_INT => InlineValue::Int(val),
        TAG_FLOAT => InlineValue::Float(f64::from_bits(val as u64)),
        TAG_BOOL => InlineValue::Bool(val != 0),
        TAG_NULL => InlineValue::Null,
        _ => InlineValue::Handle(HandleId(val as u64)),
    }
}

fn render_inline(value: &InlineValue) -> String {
    match value {
        InlineValue::Null => "null".to_owned(),
        InlineValue::Bool(v) => v.to_string(),
        InlineValue::Int(v) => v.to_string(),
        InlineValue::Float(v) => v.to_string(),
        InlineValue::String(v) => v.clone(),
        InlineValue::Handle(h) => format!("<handle {}>", h.0),
        InlineValue::Tuple(items) => format!("({})", items.iter().map(|v| render_inline(v)).collect::<Vec<_>>().join(", ")),
        InlineValue::Record(fields) => format!("{{{}}}", fields.iter().map(|(k, v)| format!("{k}: {}", render_inline(v))).collect::<Vec<_>>().join(", ")),
    }
}

fn format_args(ty: &str, items: &[InlineValue]) -> String {
    let rendered: Vec<String> = items.iter().map(render_inline).collect();
    format!("({ty} {})", rendered.join(", "))
}

fn name_to_tag(name: &str) -> i32 {
    match name {
        "Int" => TAG_INT,
        "Float" => TAG_FLOAT,
        "Bool" => TAG_BOOL,
        "String" => TAG_STRING,
        "Null" => TAG_NULL,
        _ => -1,
    }
}

fn mem_read_i32(data: &[u8], offset: u32) -> Result<i32, String> {
    let bytes = data.get(offset as usize..offset as usize + 4).ok_or("read out of bounds")?;
    Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn mem_read_i64(data: &[u8], offset: u32) -> Result<i64, String> {
    let bytes = data.get(offset as usize..offset as usize + 8).ok_or("read out of bounds")?;
    Ok(i64::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]]))
}
