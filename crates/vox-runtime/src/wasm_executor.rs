use std::collections::BTreeMap;

use wasmtime::*;

use vox_core::{
    ids::HandleId,
    source::ModulePath,
    value::{HandleData, HandleSummary, InlineValue, RuntimeValue},
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
const TAG_INVALID: i32 = -1;

#[derive(Debug)]
struct State {
    runtime: *mut Runtime,
    iterators: BTreeMap<u64, WasmIteratorState>,
}

#[derive(Debug)]
struct WasmIteratorState {
    items: Vec<HandleData>,
    position: usize,
}

pub fn try_wasm_execute(
    runtime: &mut Runtime,
    wasm_bytes: &[u8],
    arguments: &[RuntimeValue],
) -> Result<RuntimeValue, String> {
    let engine = Engine::default();
    let module = Module::new(&engine, wasm_bytes).map_err(|e| e.to_string())?;

    let runtime_ptr = runtime as *mut Runtime;

    let mut store = Store::new(
        &engine,
        State {
            runtime: runtime_ptr,
            iterators: BTreeMap::new(),
        },
    );

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

            let (runtime_ptr, iterators_ptr) = {
                let state = caller.data_mut();
                (
                    state.runtime,
                    &mut state.iterators as *mut BTreeMap<u64, WasmIteratorState>,
                )
            };
            let runtime = unsafe { &mut *runtime_ptr };
            let iterators = unsafe { &mut *iterators_ptr };

            let Some(mem) = caller.get_export("memory").and_then(|e| e.into_memory()) else {
                return Err(wasmtime::Error::msg(
                    "wasm import __vox_op: memory export not found",
                ));
            };
            clear_result_slot(&mem, &mut caller, result_ptr)?;
            let result = {
                let data = mem.data(&caller);
                handle_builtin_op(
                    runtime,
                    iterators,
                    op_id,
                    args_ptr,
                    arg_count,
                    extra_ptr,
                    extra_count,
                    data,
                )
            };
            let (tag, val) = result.map_err(|message| {
                wasmtime::Error::msg(format!("wasm import __vox_op failed: {message}"))
            })?;
            write_result_slot(&mem, &mut caller, result_ptr, tag, val)?;
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

            let Some(mem) = caller.get_export("memory").and_then(|e| e.into_memory()) else {
                return Err(wasmtime::Error::msg(
                    "wasm import __vox_host: memory export not found",
                ));
            };
            clear_result_slot(&mem, &mut caller, result_ptr)?;
            let result = {
                let data = mem.data(&caller);
                handle_host_call(runtime, callee_ptr, callee_len, args_ptr, arg_count, data)
            };
            let (tag, val) = result.map_err(|message| {
                wasmtime::Error::msg(format!("wasm import __vox_host failed: {message}"))
            })?;
            write_result_slot(&mem, &mut caller, result_ptr, tag, val)?;
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
        let (tag, val) = to_wasm(runtime, arg)?;
        mem.write(&mut store, scratch as usize + 4, &tag.to_le_bytes())
            .map_err(|e| e.to_string())?;
        mem.write(&mut store, scratch as usize + 8, &val.to_le_bytes())
            .map_err(|e| e.to_string())?;
        scratch += 16;
    }

    let entry = instance
        .get_typed_func::<i32, (i32, i64)>(&mut store, "script_entry")
        .map_err(|e| e.to_string())?;

    let (result_tag, result_data) = entry.call(&mut store, 0i32).map_err(|e| e.to_string())?;

    from_wasm(runtime, result_tag, result_data)
}

fn clear_result_slot(
    mem: &Memory,
    caller: &mut Caller<'_, State>,
    result_ptr: u32,
) -> wasmtime::Result<()> {
    write_result_slot(mem, caller, result_ptr, TAG_INVALID, 0)
}

fn write_result_slot(
    mem: &Memory,
    caller: &mut Caller<'_, State>,
    result_ptr: u32,
    tag: i32,
    val: i64,
) -> wasmtime::Result<()> {
    mem.write(&mut *caller, result_ptr as usize, &tag.to_le_bytes())
        .map_err(|error| wasmtime::Error::msg(format!("wasm result tag write failed: {error}")))?;
    mem.write(&mut *caller, result_ptr as usize + 8, &val.to_le_bytes())
        .map_err(|error| wasmtime::Error::msg(format!("wasm result data write failed: {error}")))?;
    Ok(())
}

fn handle_builtin_op(
    runtime: &mut Runtime,
    iterators: &mut BTreeMap<u64, WasmIteratorState>,
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
        1 => builtin_record_new(runtime, &args, &extra),
        2 => builtin_list_new(runtime, &args),
        3 => builtin_string_new(runtime, &extra),
        4 => builtin_string_interpolate(runtime, &args, &extra),
        5 => builtin_project(runtime, &args, &extra),
        6 => builtin_index(runtime, &args),
        7 => builtin_updated(runtime, &args, &extra),
        8 => builtin_type_test(runtime, &args, &extra),
        9 => builtin_iterator(runtime, iterators, &args),
        10 => builtin_iterator_next(runtime, iterators, &args),
        11 => builtin_lambda_new(runtime, &extra),
        12 => builtin_econ_new(runtime, &args),
        13 => builtin_non_null(&args),
        14 => builtin_safe_project(runtime, &args, &extra),
        15 => builtin_string_binary(runtime, &args, &extra),
        16 => builtin_numeric_checked(runtime, &args, &extra),
        17 => builtin_range_new(runtime, &args, &extra),
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
        arg_values.push(from_wasm(runtime, tag, val)?);
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
        return to_wasm(runtime, &result);
    }

    Err(format!("host call target not found: {callee}"))
}

fn builtin_tuple_new(runtime: &mut Runtime, args: &[(i32, i64)]) -> Result<(i32, i64), String> {
    let items: Vec<InlineValue> = args
        .iter()
        .map(|(t, v)| wasm_to_inline(runtime, *t, *v))
        .collect::<Result<_, _>>()?;
    inline_result_to_wasm(runtime, InlineValue::Tuple(items))
}

fn builtin_record_new(
    runtime: &mut Runtime,
    args: &[(i32, i64)],
    names: &[Vec<u8>],
) -> Result<(i32, i64), String> {
    if args.len() != names.len() {
        return Err(format!(
            "RecordNew expected {} field names, received {}",
            args.len(),
            names.len()
        ));
    }
    let items: Vec<InlineValue> = args
        .iter()
        .map(|(t, v)| wasm_to_inline(runtime, *t, *v))
        .collect::<Result<_, _>>()?;
    let fields = names
        .iter()
        .cloned()
        .map(|name| String::from_utf8(name).map_err(|error| format!("invalid field name: {error}")))
        .zip(items)
        .map(|(name, value)| Ok((name?, value)))
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    inline_result_to_wasm(runtime, InlineValue::Record(fields))
}

fn builtin_list_new(runtime: &mut Runtime, args: &[(i32, i64)]) -> Result<(i32, i64), String> {
    let items: Vec<InlineValue> = args
        .iter()
        .map(|(t, v)| wasm_to_inline(runtime, *t, *v))
        .collect::<Result<_, _>>()?;
    let data = HandleData::List(
        items
            .iter()
            .map(handle_data_from_inline)
            .collect::<Result<_, _>>()?,
    );
    let summary = HandleSummary {
        type_name: "List".to_owned(),
        summary: format!(
            "[{}]",
            items
                .iter()
                .map(render_inline)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        bytes: None,
    };
    let handle = runtime.allocate_serializable_handle(summary, data);
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
    let handle = runtime.allocate_serializable_handle(summary, HandleData::String(s));
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
        let v = wasm_to_inline(runtime, *tag, *val)?;
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
    let handle = runtime.allocate_serializable_handle(summary, HandleData::String(out));
    Ok((TAG_STRING, handle.0 as i64))
}

fn builtin_string_binary(
    runtime: &mut Runtime,
    args: &[(i32, i64)],
    extra: &[Vec<u8>],
) -> Result<(i32, i64), String> {
    if args.len() != 2 || extra.is_empty() {
        return Err("StringBinary expects two args and an op name".to_owned());
    }
    let op = std::str::from_utf8(&extra[0])
        .map_err(|error| format!("invalid StringBinary op name: {error}"))?;
    let left = wasm_to_data(runtime, args[0].0, args[0].1)?;
    let right = wasm_to_data(runtime, args[1].0, args[1].1)?;
    let (HandleData::String(left), HandleData::String(right)) = (left, right) else {
        return Err("StringBinary operands must be String".to_owned());
    };

    match op {
        "add" => handle_data_result_to_wasm(runtime, HandleData::String(format!("{left}{right}"))),
        "equal" => Ok((TAG_BOOL, (left == right) as i64)),
        "not_equal" => Ok((TAG_BOOL, (left != right) as i64)),
        "less" => Ok((TAG_BOOL, (left < right) as i64)),
        "greater" => Ok((TAG_BOOL, (left > right) as i64)),
        "less_equal" => Ok((TAG_BOOL, (left <= right) as i64)),
        "greater_equal" => Ok((TAG_BOOL, (left >= right) as i64)),
        other => Err(format!("unsupported StringBinary op `{other}`")),
    }
}

fn builtin_numeric_checked(
    runtime: &Runtime,
    args: &[(i32, i64)],
    extra: &[Vec<u8>],
) -> Result<(i32, i64), String> {
    if args.len() != 2 || extra.is_empty() {
        return Err("NumericChecked expects two args and an op name".to_owned());
    }
    let op = std::str::from_utf8(&extra[0])
        .map_err(|error| format!("invalid NumericChecked op name: {error}"))?;
    let left = wasm_to_inline(runtime, args[0].0, args[0].1)?;
    let right = wasm_to_inline(runtime, args[1].0, args[1].1)?;
    match (op, left, right) {
        ("divide", InlineValue::Int(_), InlineValue::Int(0)) => {
            Err("integer division by zero".to_owned())
        }
        ("remainder", InlineValue::Int(_), InlineValue::Int(0)) => {
            Err("integer remainder by zero".to_owned())
        }
        ("divide", InlineValue::Int(left), InlineValue::Int(right)) => Ok((TAG_INT, left / right)),
        ("remainder", InlineValue::Int(left), InlineValue::Int(right)) => {
            Ok((TAG_INT, left % right))
        }
        ("divide", InlineValue::Float(left), InlineValue::Float(right)) => {
            Ok((TAG_FLOAT, (left / right).to_bits() as i64))
        }
        ("remainder", InlineValue::Float(left), InlineValue::Float(right)) => {
            Ok((TAG_FLOAT, (left % right).to_bits() as i64))
        }
        ("divide", InlineValue::Int(left), InlineValue::Float(right)) => {
            Ok((TAG_FLOAT, ((left as f64) / right).to_bits() as i64))
        }
        ("remainder", InlineValue::Int(left), InlineValue::Float(right)) => {
            Ok((TAG_FLOAT, ((left as f64) % right).to_bits() as i64))
        }
        ("divide", InlineValue::Float(left), InlineValue::Int(right)) => {
            Ok((TAG_FLOAT, (left / right as f64).to_bits() as i64))
        }
        ("remainder", InlineValue::Float(left), InlineValue::Int(right)) => {
            Ok((TAG_FLOAT, (left % right as f64).to_bits() as i64))
        }
        (other, left, right) => Err(format!(
            "numeric `{other}` is not defined for {} and {}",
            inline_type_name(&left),
            inline_type_name(&right)
        )),
    }
}

fn builtin_project(
    runtime: &mut Runtime,
    args: &[(i32, i64)],
    extra: &[Vec<u8>],
) -> Result<(i32, i64), String> {
    if args.is_empty() || extra.is_empty() {
        return Err("Project missing args".to_owned());
    }
    let target = wasm_to_inline(runtime, args[0].0, args[0].1)?;
    let projection = parse_projection(&extra[0])?;
    inline_result_to_wasm(runtime, project_inline(target, &projection)?)
}

fn builtin_index(runtime: &mut Runtime, args: &[(i32, i64)]) -> Result<(i32, i64), String> {
    if args.len() != 2 {
        return Err("Index expects target and index args".to_owned());
    }
    let index = wasm_to_inline(runtime, args[1].0, args[1].1)?;
    let target = wasm_to_data(runtime, args[0].0, args[0].1)?;
    handle_data_result_to_wasm(runtime, index_data(target, index)?)
}

fn builtin_updated(
    runtime: &mut Runtime,
    args: &[(i32, i64)],
    extra: &[Vec<u8>],
) -> Result<(i32, i64), String> {
    if args.len() != 2 || extra.is_empty() {
        return Err("Updated expects target, replacement, and path data".to_owned());
    }
    let target = wasm_to_data(runtime, args[0].0, args[0].1)?;
    let replacement = wasm_to_data(runtime, args[1].0, args[1].1)?;
    let path = parse_update_path(&extra[0])?;
    handle_data_result_to_wasm(runtime, update_data(target, &path, replacement)?)
}

fn builtin_type_test(
    runtime: &Runtime,
    args: &[(i32, i64)],
    extra: &[Vec<u8>],
) -> Result<(i32, i64), String> {
    if extra.is_empty() || args.is_empty() {
        return Err("TypeTest missing data".to_owned());
    }
    let ty = String::from_utf8(extra[0].clone()).unwrap_or_default();
    let result = if wasm_matches_type(runtime, args[0].0, args[0].1, &ty) {
        1i64
    } else {
        0i64
    };
    Ok((TAG_BOOL, result))
}

fn builtin_iterator(
    runtime: &mut Runtime,
    iterators: &mut BTreeMap<u64, WasmIteratorState>,
    args: &[(i32, i64)],
) -> Result<(i32, i64), String> {
    if args.is_empty() {
        return Err("Iterator missing argument".to_owned());
    }
    let iterable = wasm_to_data(runtime, args[0].0, args[0].1)?;
    let items = expand_wasm_iterable(&iterable)?;
    let iterator_id = runtime
        .allocate_serializable_handle(
            HandleSummary {
                type_name: "Iterator".to_owned(),
                summary: format!("<iterator {} items>", items.len()),
                bytes: None,
            },
            HandleData::Null,
        )
        .0;
    iterators.insert(
        iterator_id,
        WasmIteratorState {
            items,
            position: 0,
        },
    );
    Ok((TAG_HANDLE, iterator_id as i64))
}

fn builtin_iterator_next(
    runtime: &mut Runtime,
    iterators: &mut BTreeMap<u64, WasmIteratorState>,
    args: &[(i32, i64)],
) -> Result<(i32, i64), String> {
    if args.is_empty() {
        return Err("IteratorNext missing argument".to_owned());
    }
    let iterator_id = match wasm_to_inline(runtime, args[0].0, args[0].1)? {
        InlineValue::Handle(handle) => handle.0,
        _ => return Err("IteratorNext expects an iterator handle".to_owned()),
    };
    let state = iterators
        .get_mut(&iterator_id)
        .ok_or_else(|| format!("IteratorNext: unknown iterator {iterator_id}"))?;
    if state.position < state.items.len() {
        let item = state.items[state.position].clone();
        state.position += 1;
        handle_data_result_to_wasm(runtime, item)
    } else {
        Ok((TAG_NULL, 0))
    }
}

fn builtin_range_new(
    runtime: &mut Runtime,
    args: &[(i32, i64)],
    extra: &[Vec<u8>],
) -> Result<(i32, i64), String> {
    if extra.is_empty() {
        return Err("RangeNew missing op name".to_owned());
    }
    let op = std::str::from_utf8(&extra[0])
        .map_err(|error| format!("invalid RangeNew op name: {error}"))?;
    let inclusive = op == "range_inclusive";
    if args.is_empty() {
        return Err("range requires at least one bound".to_owned());
    }
    let start = wasm_to_inline(runtime, args[0].0, args[0].1)?;
    let end = if args.len() >= 2 {
        Some(wasm_to_inline(runtime, args[1].0, args[1].1)?)
    } else {
        None
    };
    let start_int = match start {
        InlineValue::Int(v) => v,
        _ => return Err("Range bounds must be Int".to_owned()),
    };
    let end_int = match &end {
        Some(InlineValue::Int(v)) => *v,
        Some(_) => return Err("Range bounds must be Int".to_owned()),
        None => i64::MAX,
    };
    let summary_text = if let Some(InlineValue::Int(e)) = &end {
        if inclusive {
            format!("{start_int}..={e}")
        } else {
            format!("{start_int}..{e}")
        }
    } else {
        format!("{start_int}..")
    };
    let summary = HandleSummary {
        type_name: "Range".to_owned(),
        summary: summary_text,
        bytes: None,
    };
    let handle = runtime.allocate_serializable_handle(
        summary,
        HandleData::Tuple(vec![
            HandleData::Int(start_int),
            HandleData::Int(end_int),
            HandleData::Bool(inclusive),
        ]),
    );
    Ok((TAG_HANDLE, handle.0 as i64))
}

fn expand_wasm_iterable(data: &HandleData) -> Result<Vec<HandleData>, String> {
    match data {
        HandleData::List(items) => Ok(items.clone()),
        HandleData::Tuple(items) if items.len() == 3 => {
            if let (
                HandleData::Int(start),
                HandleData::Int(end),
                HandleData::Bool(inclusive),
            ) = (&items[0], &items[1], &items[2])
            {
                let end_bound = if *inclusive { *end + 1 } else { *end };
                let count = (end_bound - *start).max(0) as usize;
                Ok((0..count)
                    .map(|i| HandleData::Int(*start + i as i64))
                    .collect())
            } else {
                Err("range requires Int bounds".to_owned())
            }
        }
        _ => Err(format!(
            "iteration is not supported for {}",
            handle_data_type_name(data)
        )),
    }
}

fn builtin_lambda_new(runtime: &mut Runtime, extra: &[Vec<u8>]) -> Result<(i32, i64), String> {
    let params: Vec<String> = extra
        .iter()
        .map(|s| String::from_utf8_lossy(s).to_string())
        .collect();
    let sig = if params.is_empty() {
        "()".to_owned()
    } else {
        params.join(", ")
    };
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
    let v = wasm_to_inline(runtime, args[0].0, args[0].1)?;
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

fn builtin_safe_project(
    runtime: &mut Runtime,
    args: &[(i32, i64)],
    extra: &[Vec<u8>],
) -> Result<(i32, i64), String> {
    if args.is_empty() {
        return Err("SafeProject missing arg".to_owned());
    }
    if args[0].0 == TAG_NULL {
        return Ok((TAG_NULL, 0));
    }
    builtin_project(runtime, args, extra)
}

fn to_wasm(runtime: &mut Runtime, value: &RuntimeValue) -> Result<(i32, i64), String> {
    match value {
        RuntimeValue::Inline(iv) => inline_result_to_wasm(runtime, iv.clone()),
        RuntimeValue::Handle(handle) => Ok(handle_to_wasm(runtime, *handle)),
    }
}

fn handle_to_wasm(runtime: &Runtime, handle: HandleId) -> (i32, i64) {
    let tag = match runtime
        .describe_handle(handle)
        .map(|summary| summary.type_name)
    {
        Some(type_name) if type_name == "String" || type_name.ends_with(".String") => TAG_STRING,
        Some(type_name) if type_name == "Tuple" || type_name.ends_with(".Tuple") => TAG_TUPLE,
        Some(type_name) if type_name == "Record" || type_name.ends_with(".Record") => TAG_RECORD,
        Some(type_name) if type_name == "List" || type_name.ends_with(".List") => TAG_LIST,
        _ => TAG_HANDLE,
    };
    (tag, handle.0 as i64)
}

fn from_wasm(runtime: &Runtime, tag: i32, val: i64) -> Result<RuntimeValue, String> {
    match tag {
        TAG_INT => Ok(RuntimeValue::Inline(InlineValue::Int(val))),
        TAG_FLOAT => Ok(RuntimeValue::Inline(InlineValue::Float(f64::from_bits(
            val as u64,
        )))),
        TAG_BOOL => Ok(RuntimeValue::Inline(InlineValue::Bool(val != 0))),
        TAG_NULL => Ok(RuntimeValue::Inline(InlineValue::Null)),
        TAG_STRING | TAG_TUPLE | TAG_RECORD => {
            let handle = HandleId(val as u64);
            match runtime.get_handle_data(handle) {
                Ok(data) => handle_data_to_inline(data).map(RuntimeValue::Inline),
                Err(_) => Ok(RuntimeValue::Handle(handle)),
            }
        }
        TAG_HANDLE | TAG_LIST => Ok(RuntimeValue::Handle(HandleId(val as u64))),
        _ => Err(format!("unknown wasm result tag {tag}")),
    }
}

fn wasm_to_inline(runtime: &Runtime, tag: i32, val: i64) -> Result<InlineValue, String> {
    match tag {
        TAG_INT => Ok(InlineValue::Int(val)),
        TAG_FLOAT => Ok(InlineValue::Float(f64::from_bits(val as u64))),
        TAG_BOOL => Ok(InlineValue::Bool(val != 0)),
        TAG_NULL => Ok(InlineValue::Null),
        TAG_STRING | TAG_TUPLE | TAG_RECORD => {
            let handle = HandleId(val as u64);
            match runtime.get_handle_data(handle) {
                Ok(data) => handle_data_to_inline(data),
                Err(_) => Ok(InlineValue::Handle(handle)),
            }
        }
        TAG_HANDLE | TAG_LIST => Ok(InlineValue::Handle(HandleId(val as u64))),
        _ => Err(format!("unknown wasm value tag {tag}")),
    }
}

fn inline_result_to_wasm(runtime: &mut Runtime, value: InlineValue) -> Result<(i32, i64), String> {
    match value {
        InlineValue::Int(value) => Ok((TAG_INT, value)),
        InlineValue::Float(value) => Ok((TAG_FLOAT, value.to_bits() as i64)),
        InlineValue::Bool(value) => Ok((TAG_BOOL, value as i64)),
        InlineValue::Null => Ok((TAG_NULL, 0)),
        InlineValue::Handle(handle) => Ok((TAG_HANDLE, handle.0 as i64)),
        InlineValue::String(value) => {
            let summary = HandleSummary {
                type_name: "String".to_owned(),
                summary: value.clone(),
                bytes: Some(value.len() as u64),
            };
            let handle = runtime.allocate_serializable_handle(summary, HandleData::String(value));
            Ok((TAG_STRING, handle.0 as i64))
        }
        InlineValue::Tuple(items) => {
            let summary = HandleSummary {
                type_name: "Tuple".to_owned(),
                summary: render_inline(&InlineValue::Tuple(items.clone())),
                bytes: None,
            };
            let data = HandleData::Tuple(
                items
                    .iter()
                    .map(handle_data_from_inline)
                    .collect::<Result<_, _>>()?,
            );
            let handle = runtime.allocate_serializable_handle(summary, data);
            Ok((TAG_TUPLE, handle.0 as i64))
        }
        InlineValue::Record(fields) => {
            let summary = HandleSummary {
                type_name: "Record".to_owned(),
                summary: render_inline(&InlineValue::Record(fields.clone())),
                bytes: None,
            };
            let data = HandleData::Record(
                fields
                    .iter()
                    .map(|(name, value)| Ok((name.clone(), handle_data_from_inline(value)?)))
                    .collect::<Result<_, String>>()?,
            );
            let handle = runtime.allocate_serializable_handle(summary, data);
            Ok((TAG_RECORD, handle.0 as i64))
        }
    }
}

fn wasm_to_data(runtime: &Runtime, tag: i32, val: i64) -> Result<HandleData, String> {
    match tag {
        TAG_INT => Ok(HandleData::Int(val)),
        TAG_FLOAT => Ok(HandleData::Float(f64::from_bits(val as u64))),
        TAG_BOOL => Ok(HandleData::Bool(val != 0)),
        TAG_NULL => Ok(HandleData::Null),
        TAG_STRING | TAG_TUPLE | TAG_RECORD | TAG_LIST | TAG_HANDLE => {
            runtime.get_handle_data(HandleId(val as u64))
        }
        _ => Err(format!("unknown wasm value tag {tag}")),
    }
}

fn handle_data_result_to_wasm(
    runtime: &mut Runtime,
    data: HandleData,
) -> Result<(i32, i64), String> {
    match data {
        HandleData::Null => Ok((TAG_NULL, 0)),
        HandleData::Bool(value) => Ok((TAG_BOOL, value as i64)),
        HandleData::Int(value) => Ok((TAG_INT, value)),
        HandleData::Float(value) => Ok((TAG_FLOAT, value.to_bits() as i64)),
        HandleData::String(value) => {
            let summary = HandleSummary {
                type_name: "String".to_owned(),
                summary: value.clone(),
                bytes: Some(value.len() as u64),
            };
            let handle = runtime.allocate_serializable_handle(summary, HandleData::String(value));
            Ok((TAG_STRING, handle.0 as i64))
        }
        HandleData::Tuple(values) => {
            let data = HandleData::Tuple(values);
            let summary = HandleSummary {
                type_name: "Tuple".to_owned(),
                summary: render_handle_data(&data),
                bytes: None,
            };
            let handle = runtime.allocate_serializable_handle(summary, data);
            Ok((TAG_TUPLE, handle.0 as i64))
        }
        HandleData::Record(fields) => {
            let data = HandleData::Record(fields);
            let summary = HandleSummary {
                type_name: "Record".to_owned(),
                summary: render_handle_data(&data),
                bytes: None,
            };
            let handle = runtime.allocate_serializable_handle(summary, data);
            Ok((TAG_RECORD, handle.0 as i64))
        }
        HandleData::List(values) => {
            let data = HandleData::List(values);
            let summary = HandleSummary {
                type_name: "List".to_owned(),
                summary: render_handle_data(&data),
                bytes: None,
            };
            let handle = runtime.allocate_serializable_handle(summary, data);
            Ok((TAG_LIST, handle.0 as i64))
        }
    }
}

fn handle_data_to_inline(data: HandleData) -> Result<InlineValue, String> {
    match data {
        HandleData::Null => Ok(InlineValue::Null),
        HandleData::Bool(value) => Ok(InlineValue::Bool(value)),
        HandleData::Int(value) => Ok(InlineValue::Int(value)),
        HandleData::Float(value) => Ok(InlineValue::Float(value)),
        HandleData::String(value) => Ok(InlineValue::String(value)),
        HandleData::Tuple(values) => values
            .into_iter()
            .map(handle_data_to_inline)
            .collect::<Result<Vec<_>, _>>()
            .map(InlineValue::Tuple),
        HandleData::Record(fields) => fields
            .into_iter()
            .map(|(name, value)| Ok((name, handle_data_to_inline(value)?)))
            .collect::<Result<BTreeMap<_, _>, String>>()
            .map(InlineValue::Record),
        HandleData::List(_) => Err("list handle data has no inline Vox value".to_owned()),
    }
}

fn handle_data_from_inline(value: &InlineValue) -> Result<HandleData, String> {
    match value {
        InlineValue::Null => Ok(HandleData::Null),
        InlineValue::Bool(value) => Ok(HandleData::Bool(*value)),
        InlineValue::Int(value) => Ok(HandleData::Int(*value)),
        InlineValue::Float(value) => Ok(HandleData::Float(*value)),
        InlineValue::String(value) => Ok(HandleData::String(value.clone())),
        InlineValue::Tuple(values) => values
            .iter()
            .map(handle_data_from_inline)
            .collect::<Result<Vec<_>, _>>()
            .map(HandleData::Tuple),
        InlineValue::Record(fields) => fields
            .iter()
            .map(|(name, value)| Ok((name.clone(), handle_data_from_inline(value)?)))
            .collect::<Result<BTreeMap<_, _>, String>>()
            .map(HandleData::Record),
        InlineValue::Handle(handle) => Err(format!(
            "handle {} does not expose inline data in wasm result",
            handle.0
        )),
    }
}

fn wasm_matches_type(runtime: &Runtime, tag: i32, val: i64, ty: &str) -> bool {
    match (ty, tag) {
        ("Int", TAG_INT)
        | ("Float", TAG_FLOAT)
        | ("Bool", TAG_BOOL)
        | ("String", TAG_STRING)
        | ("Null", TAG_NULL)
        | ("Tuple", TAG_TUPLE)
        | ("Record", TAG_RECORD)
        | ("List", TAG_LIST) => return true,
        ("Unit", TAG_TUPLE) => {
            return matches!(
                runtime.get_handle_data(HandleId(val as u64)),
                Ok(HandleData::Tuple(items)) if items.is_empty()
            );
        }
        _ => {}
    }

    if let Some(expected_tag) = primitive_type_tag(ty) {
        return tag == expected_tag;
    }

    let handle = match tag {
        TAG_HANDLE | TAG_STRING | TAG_TUPLE | TAG_RECORD | TAG_LIST => HandleId(val as u64),
        _ => return false,
    };

    let Some(summary) = runtime.describe_handle(handle) else {
        return false;
    };
    let handle_type = summary.type_name;

    if type_name_matches(&handle_type, ty) {
        return true;
    }

    runtime.host.packages().any(|manifest| {
        manifest.trait_impls.iter().any(|(trait_qt, impl_types)| {
            let full_trait_name = format!("{}.{}", trait_qt.module.as_str(), trait_qt.name);
            if !type_name_matches(&full_trait_name, ty) && trait_qt.name != ty {
                return false;
            }
            impl_types.iter().any(|impl_qt| {
                let full_impl_name = format!("{}.{}", impl_qt.module.as_str(), impl_qt.name);
                type_name_matches(&handle_type, &full_impl_name) || handle_type == impl_qt.name
            })
        })
    })
}

fn primitive_type_tag(ty: &str) -> Option<i32> {
    match ty {
        "Int" => Some(TAG_INT),
        "Float" => Some(TAG_FLOAT),
        "Bool" => Some(TAG_BOOL),
        "String" => Some(TAG_STRING),
        "Null" => Some(TAG_NULL),
        "Tuple" => Some(TAG_TUPLE),
        "Record" => Some(TAG_RECORD),
        "List" => Some(TAG_LIST),
        _ => None,
    }
}

fn type_name_matches(actual: &str, expected: &str) -> bool {
    actual == expected
        || actual.ends_with(&format!(".{expected}"))
        || expected.ends_with(&format!(".{actual}"))
}

#[derive(Debug, Clone)]
enum RuntimeProjection {
    Field(String),
    Slot(usize),
}

#[derive(Debug, Clone)]
enum RuntimePathSegment {
    Field(String),
    Index(usize),
}

fn parse_projection(data: &[u8]) -> Result<RuntimeProjection, String> {
    let Some((&kind, rest)) = data.split_first() else {
        return Err("projection data is empty".to_owned());
    };
    match kind {
        0 => {
            let len = read_u32_from(rest, 0)? as usize;
            let start = 4usize;
            let end = start + len;
            let bytes = rest
                .get(start..end)
                .ok_or_else(|| "projection field data out of bounds".to_owned())?;
            let field = String::from_utf8(bytes.to_vec())
                .map_err(|error| format!("invalid projection field: {error}"))?;
            Ok(RuntimeProjection::Field(field))
        }
        1 => Ok(RuntimeProjection::Slot(read_u32_from(rest, 0)? as usize)),
        _ => Err(format!("unknown projection kind {kind}")),
    }
}

fn parse_update_path(data: &[u8]) -> Result<Vec<RuntimePathSegment>, String> {
    let count = read_u32_from(data, 0)? as usize;
    let mut offset = 4usize;
    let mut path = Vec::new();
    for _ in 0..count {
        let kind = *data
            .get(offset)
            .ok_or_else(|| "updated path segment out of bounds".to_owned())?;
        offset += 1;
        match kind {
            0 => {
                let len = read_u32_from(data, offset)? as usize;
                offset += 4;
                let end = offset + len;
                let bytes = data
                    .get(offset..end)
                    .ok_or_else(|| "updated field data out of bounds".to_owned())?;
                offset = end;
                path.push(RuntimePathSegment::Field(
                    String::from_utf8(bytes.to_vec())
                        .map_err(|error| format!("invalid updated field: {error}"))?,
                ));
            }
            1 => {
                let index = read_u32_from(data, offset)? as usize;
                offset += 4;
                path.push(RuntimePathSegment::Index(index));
            }
            _ => return Err(format!("unknown updated path segment kind {kind}")),
        }
    }
    Ok(path)
}

fn project_inline(
    target: InlineValue,
    projection: &RuntimeProjection,
) -> Result<InlineValue, String> {
    match (target, projection) {
        (InlineValue::Record(fields), RuntimeProjection::Field(field)) => fields
            .get(field)
            .cloned()
            .ok_or_else(|| format!("record does not contain field `{field}`")),
        (InlineValue::Tuple(items), RuntimeProjection::Slot(slot)) => items
            .get(*slot)
            .cloned()
            .ok_or_else(|| format!("tuple index {slot} is out of bounds")),
        (other, RuntimeProjection::Field(field)) => Err(format!(
            "field `{field}` is not supported for {}",
            inline_type_name(&other)
        )),
        (other, RuntimeProjection::Slot(slot)) => Err(format!(
            "slot `{slot}` is not supported for {}",
            inline_type_name(&other)
        )),
    }
}

fn index_data(target: HandleData, index: InlineValue) -> Result<HandleData, String> {
    let InlineValue::Int(index) = index else {
        return Err("index expressions require an Int index".to_owned());
    };
    let index = usize::try_from(index)
        .map_err(|_| "index expressions require a non-negative index".to_owned())?;
    match target {
        HandleData::Tuple(items) => items
            .get(index)
            .cloned()
            .ok_or_else(|| format!("tuple index {index} is out of bounds")),
        HandleData::List(items) => items
            .get(index)
            .cloned()
            .ok_or_else(|| format!("list index {index} is out of bounds")),
        other => Err(format!(
            "indexing is not supported for {}",
            handle_data_type_name(&other)
        )),
    }
}

fn update_data(
    target: HandleData,
    path: &[RuntimePathSegment],
    replacement: HandleData,
) -> Result<HandleData, String> {
    let Some((segment, rest)) = path.split_first() else {
        return Err("updated path cannot be empty".to_owned());
    };
    match (target, segment) {
        (HandleData::Record(mut fields), RuntimePathSegment::Field(name)) => {
            let current = fields
                .get(name)
                .cloned()
                .ok_or_else(|| format!("record does not contain field `{name}`"))?;
            let next = if rest.is_empty() {
                replacement
            } else {
                update_data(current, rest, replacement)?
            };
            fields.insert(name.clone(), next);
            Ok(HandleData::Record(fields))
        }
        (HandleData::Tuple(mut items), RuntimePathSegment::Index(index)) => {
            let slot = items
                .get_mut(*index)
                .ok_or_else(|| format!("tuple index {index} is out of bounds"))?;
            *slot = if rest.is_empty() {
                replacement
            } else {
                update_data(slot.clone(), rest, replacement)?
            };
            Ok(HandleData::Tuple(items))
        }
        (HandleData::List(mut items), RuntimePathSegment::Index(index)) => {
            let slot = items
                .get_mut(*index)
                .ok_or_else(|| format!("list index {index} is out of bounds"))?;
            *slot = if rest.is_empty() {
                replacement
            } else {
                update_data(slot.clone(), rest, replacement)?
            };
            Ok(HandleData::List(items))
        }
        (other, _) => Err(format!(
            "updated is not supported for {}",
            handle_data_type_name(&other)
        )),
    }
}

fn read_u32_from(data: &[u8], offset: usize) -> Result<u32, String> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or_else(|| "metadata read out of bounds".to_owned())?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn inline_type_name(value: &InlineValue) -> &'static str {
    match value {
        InlineValue::Null => "Null",
        InlineValue::Bool(_) => "Bool",
        InlineValue::Int(_) => "Int",
        InlineValue::Float(_) => "Float",
        InlineValue::String(_) => "String",
        InlineValue::Tuple(_) => "Tuple",
        InlineValue::Record(_) => "Record",
        InlineValue::Handle(_) => "Handle",
    }
}

fn handle_data_type_name(value: &HandleData) -> &'static str {
    match value {
        HandleData::Null => "Null",
        HandleData::Bool(_) => "Bool",
        HandleData::Int(_) => "Int",
        HandleData::Float(_) => "Float",
        HandleData::String(_) => "String",
        HandleData::Tuple(_) => "Tuple",
        HandleData::Record(_) => "Record",
        HandleData::List(_) => "List",
    }
}

fn render_handle_data(value: &HandleData) -> String {
    match value {
        HandleData::Null => "null".to_owned(),
        HandleData::Bool(value) => value.to_string(),
        HandleData::Int(value) => value.to_string(),
        HandleData::Float(value) => value.to_string(),
        HandleData::String(value) => value.clone(),
        HandleData::Tuple(values) => format!(
            "({})",
            values
                .iter()
                .map(render_handle_data)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        HandleData::Record(fields) => format!(
            "{{{}}}",
            fields
                .iter()
                .map(|(name, value)| format!("{name}: {}", render_handle_data(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        HandleData::List(values) => format!(
            "[{}]",
            values
                .iter()
                .map(render_handle_data)
                .collect::<Vec<_>>()
                .join(", ")
        ),
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
        InlineValue::Tuple(items) => format!(
            "({})",
            items
                .iter()
                .map(|v| render_inline(v))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        InlineValue::Record(fields) => format!(
            "{{{}}}",
            fields
                .iter()
                .map(|(k, v)| format!("{k}: {}", render_inline(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn mem_read_i32(data: &[u8], offset: u32) -> Result<i32, String> {
    let bytes = data
        .get(offset as usize..offset as usize + 4)
        .ok_or("read out of bounds")?;
    Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn mem_read_i64(data: &[u8], offset: u32) -> Result<i64, String> {
    let bytes = data
        .get(offset as usize..offset as usize + 8)
        .ok_or("read out of bounds")?;
    Ok(i64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}
