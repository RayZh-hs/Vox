use std::collections::BTreeMap;

use wasmtime::*;

use vox_core::{
    builtins::{self, BuiltinReceiver},
    ids::HandleId,
    source::ModulePath,
    value::{HandleData, HandleSummary, InlineValue, RuntimeValue},
};

use crate::{
    HostCallArgument, Runtime,
    interpreter::{self, CallArgument, Value},
};

const TAG_INT: i32 = 0;
const TAG_FLOAT: i32 = 1;
const TAG_BOOL: i32 = 2;
const TAG_STRING: i32 = 3;
const TAG_TUPLE: i32 = 4;
const TAG_RECORD: i32 = 5;
const TAG_LIST: i32 = 6;
const TAG_HANDLE: i32 = 7;
const TAG_NULL: i32 = 8;
const TAG_UINT: i32 = 9;
const TAG_INVALID: i32 = -1;
const STRDATA_OFF: i64 = 32768;
const WASM_PAGE_SIZE: u32 = 65536;
const HEAP_GUARD_BYTES: u32 = 4096;
const INITIAL_MEMORY_PAGES: u32 = 256;
const HEAP_LIMIT: u32 = INITIAL_MEMORY_PAGES * WASM_PAGE_SIZE - HEAP_GUARD_BYTES;
const MAX_MEMORY_VALUE_DEPTH: usize = 64;

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
    let mut config = Config::new();
    config.max_wasm_stack(8 * 1024 * 1024);
    let engine = Engine::new(&config).map_err(|e| e.to_string())?;
    let module = Module::new(&engine, wasm_bytes).map_err(|e| e.to_string())?;

    let runtime_ptr = runtime as *mut Runtime;

    let mut store = Store::new(
        &engine,
        State {
            runtime: runtime_ptr,
            iterators: BTreeMap::new(),
        },
    );

    let memory_ty = MemoryType::new(INITIAL_MEMORY_PAGES, None);
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
    let heap_top = instance
        .get_global(&mut store, "__vox_heap_top")
        .ok_or_else(|| "__vox_heap_top export not found".to_owned())?;

    let mut entry_args = Vec::with_capacity(arguments.len() * 2);
    for arg in arguments {
        let (tag, val) = to_wasm_entry(runtime, &mem, &heap_top, &mut store, arg)?;
        entry_args.push(Val::I32(tag));
        entry_args.push(Val::I64(val));
    }

    let entry = instance
        .get_func(&mut store, "script_entry")
        .ok_or_else(|| "script_entry export not found".to_owned())?;
    let mut results = [Val::I32(TAG_INVALID), Val::I64(0)];
    entry
        .call(&mut store, &entry_args, &mut results)
        .map_err(|e| e.to_string())?;

    let result_tag = results[0].unwrap_i32();
    let result_data = results[1].unwrap_i64();

    let data = mem.data(&store);
    from_wasm(runtime, Some(data), result_tag, result_data)
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
        0 => builtin_tuple_new(runtime, data, &args),
        1 => builtin_record_new(runtime, data, &args, &extra),
        2 => builtin_list_new(runtime, data, &args),
        3 => builtin_string_new(runtime, &extra),
        4 => builtin_string_interpolate(runtime, data, &args, &extra),
        5 => builtin_project(runtime, data, &args, &extra),
        6 => builtin_index(runtime, data, &args),
        7 => builtin_updated(runtime, data, &args, &extra),
        8 => builtin_type_test(runtime, data, &args, &extra),
        9 => builtin_iterator(runtime, iterators, data, &args),
        10 => builtin_iterator_next(runtime, iterators, data, &args),
        11 => builtin_lambda_new(runtime, &extra),
        12 => builtin_econ_new(runtime, data, &args),
        13 => builtin_non_null(&args),
        14 => builtin_safe_project(runtime, data, &args, &extra),
        15 => builtin_string_binary(runtime, data, &args, &extra),
        16 => builtin_numeric_checked(runtime, data, &args, &extra),
        17 => builtin_range_new(runtime, data, &args, &extra),
        18 => Err("wasm heap exhausted before stack guard region".to_owned()),
        19 => builtin_method(runtime, data, &args, &extra),
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
        arg_values.push(from_wasm(runtime, Some(data), tag, val)?);
    }

    if callee == "__dyn" {
        return handle_dynamic_call(runtime, &arg_values);
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

fn handle_dynamic_call(
    runtime: &mut Runtime,
    arg_values: &[RuntimeValue],
) -> Result<(i32, i64), String> {
    let (callee, call_args) = arg_values
        .split_first()
        .ok_or("dynamic call requires at least a callee value")?;
    let callee_value = interpreter::value_from_runtime_value(runtime, callee)
        .map_err(|e| format!("failed to convert callee value: {e}"))?;
    let callable = match &callee_value {
        Value::Function(function) => function.clone(),
        _ => {
            return Err("dynamic call target is not a function value".to_owned());
        }
    };
    let mut arguments: Vec<CallArgument> = Vec::with_capacity(call_args.len());
    for (i, arg) in call_args.iter().enumerate() {
        let v = interpreter::value_from_runtime_value(runtime, arg)
            .map_err(|e| format!("failed to convert argument {i}: {e}"))?;
        arguments.push(CallArgument::Positional(v));
    }
    let result = callable
        .call(runtime, arguments)
        .map_err(|e| format!("dynamic call failed: {e:?}"))?;
    let rt_value = interpreter::runtime_value_from_value(runtime, result)
        .map_err(|e| format!("failed to convert result: {e}"))?;
    to_wasm(runtime, &rt_value)
}

fn builtin_tuple_new(
    runtime: &mut Runtime,
    memory: &[u8],
    args: &[(i32, i64)],
) -> Result<(i32, i64), String> {
    let items: Vec<InlineValue> = args
        .iter()
        .map(|(t, v)| wasm_to_inline(runtime, Some(memory), *t, *v))
        .collect::<Result<_, _>>()?;
    inline_result_to_wasm(runtime, InlineValue::Tuple(items))
}

fn builtin_record_new(
    runtime: &mut Runtime,
    memory: &[u8],
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
        .map(|(t, v)| wasm_to_inline(runtime, Some(memory), *t, *v))
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

fn builtin_list_new(
    runtime: &mut Runtime,
    memory: &[u8],
    args: &[(i32, i64)],
) -> Result<(i32, i64), String> {
    let items: Vec<InlineValue> = args
        .iter()
        .map(|(t, v)| wasm_to_inline(runtime, Some(memory), *t, *v))
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
    memory: &[u8],
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
        let v = wasm_to_inline(runtime, Some(memory), *tag, *val)?;
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
    memory: &[u8],
    args: &[(i32, i64)],
    extra: &[Vec<u8>],
) -> Result<(i32, i64), String> {
    if args.len() != 2 || extra.is_empty() {
        return Err("StringBinary expects two args and an op name".to_owned());
    }
    let op = std::str::from_utf8(&extra[0])
        .map_err(|error| format!("invalid StringBinary op name: {error}"))?;
    let left = wasm_to_data(runtime, Some(memory), args[0].0, args[0].1)?;
    let right = wasm_to_data(runtime, Some(memory), args[1].0, args[1].1)?;
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
    memory: &[u8],
    args: &[(i32, i64)],
    extra: &[Vec<u8>],
) -> Result<(i32, i64), String> {
    if args.len() != 2 || extra.is_empty() {
        return Err("NumericChecked expects two args and an op name".to_owned());
    }
    let op = std::str::from_utf8(&extra[0])
        .map_err(|error| format!("invalid NumericChecked op name: {error}"))?;
    let left = wasm_to_inline(runtime, Some(memory), args[0].0, args[0].1)?;
    let right = wasm_to_inline(runtime, Some(memory), args[1].0, args[1].1)?;
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

fn builtin_method(
    runtime: &mut Runtime,
    memory: &[u8],
    args: &[(i32, i64)],
    extra: &[Vec<u8>],
) -> Result<(i32, i64), String> {
    if args.is_empty() || extra.is_empty() {
        return Err("BuiltinMethod expects a callee name and receiver".to_owned());
    }
    let callee = std::str::from_utf8(&extra[0])
        .map_err(|error| format!("invalid builtin method callee: {error}"))?;
    let (receiver, method) = builtins::split_builtin_callee(callee)
        .ok_or_else(|| format!("invalid builtin method callee `{callee}`"))?;
    match receiver {
        BuiltinReceiver::Int => {
            if args[0].0 != TAG_INT {
                return Err("Int builtin receiver mismatch".to_owned());
            }
            expect_builtin_arg_count(method, args, 1)?;
            match method {
                "toString" => {
                    handle_data_result_to_wasm(runtime, HandleData::String(args[0].1.to_string()))
                }
                "toFloat" => Ok((TAG_FLOAT, (args[0].1 as f64).to_bits() as i64)),
                "toUInt" => Ok(if args[0].1 < 0 {
                    (TAG_NULL, 0)
                } else {
                    (TAG_UINT, args[0].1)
                }),
                _ => unknown_wasm_builtin(receiver, method),
            }
        }
        BuiltinReceiver::UInt => {
            if args[0].0 != TAG_UINT {
                return Err("UInt builtin receiver mismatch".to_owned());
            }
            expect_builtin_arg_count(method, args, 1)?;
            let value = args[0].1 as u64;
            match method {
                "toString" => {
                    handle_data_result_to_wasm(runtime, HandleData::String(value.to_string()))
                }
                "toFloat" => Ok((TAG_FLOAT, (value as f64).to_bits() as i64)),
                "toInt" => Ok(i64::try_from(value)
                    .map(|value| (TAG_INT, value))
                    .unwrap_or((TAG_NULL, 0))),
                _ => unknown_wasm_builtin(receiver, method),
            }
        }
        BuiltinReceiver::Float => {
            if args[0].0 != TAG_FLOAT {
                return Err("Float builtin receiver mismatch".to_owned());
            }
            expect_builtin_arg_count(method, args, 1)?;
            let value = f64::from_bits(args[0].1 as u64);
            match method {
                "toString" => {
                    handle_data_result_to_wasm(runtime, HandleData::String(value.to_string()))
                }
                "toInt" => Ok(float_to_int_wasm(value)),
                "round" => {
                    checked_f64_to_i64_wasm((value + 0.5).floor()).map(|value| (TAG_INT, value))
                }
                "floor" => checked_f64_to_i64_wasm(value.floor()).map(|value| (TAG_INT, value)),
                "ceil" => checked_f64_to_i64_wasm(value.ceil()).map(|value| (TAG_INT, value)),
                _ => unknown_wasm_builtin(receiver, method),
            }
        }
        BuiltinReceiver::Bool => {
            if args[0].0 != TAG_BOOL {
                return Err("Bool builtin receiver mismatch".to_owned());
            }
            expect_builtin_arg_count(method, args, 1)?;
            match method {
                "toString" => handle_data_result_to_wasm(
                    runtime,
                    HandleData::String((args[0].1 != 0).to_string()),
                ),
                _ => unknown_wasm_builtin(receiver, method),
            }
        }
        BuiltinReceiver::String => {
            let value = wasm_string_arg(runtime, memory, args[0])?;
            eval_string_builtin_wasm(runtime, memory, &value, method, &args[1..])
        }
        BuiltinReceiver::List => {
            let items = wasm_list_arg(runtime, memory, args[0])?;
            eval_list_builtin_wasm(runtime, memory, &items, method, &args[1..])
        }
        BuiltinReceiver::Econ => unknown_wasm_builtin(receiver, method),
    }
}

fn eval_string_builtin_wasm(
    runtime: &mut Runtime,
    memory: &[u8],
    value: &str,
    method: &str,
    args: &[(i32, i64)],
) -> Result<(i32, i64), String> {
    match method {
        "length" => {
            expect_builtin_arg_count(method, args, 0)?;
            Ok((TAG_INT, value.chars().count() as i64))
        }
        "isEmpty" => {
            expect_builtin_arg_count(method, args, 0)?;
            Ok((TAG_BOOL, value.is_empty() as i64))
        }
        "toInt" => {
            expect_builtin_arg_count(method, args, 0)?;
            Ok(value
                .parse::<i64>()
                .map(|value| (TAG_INT, value))
                .unwrap_or((TAG_NULL, 0)))
        }
        "toFloat" => {
            expect_builtin_arg_count(method, args, 0)?;
            Ok(value
                .parse::<f64>()
                .map(|value| (TAG_FLOAT, value.to_bits() as i64))
                .unwrap_or((TAG_NULL, 0)))
        }
        "startsWith" => {
            let prefix = wasm_string_method_arg(runtime, memory, method, args, 0)?;
            Ok((TAG_BOOL, value.starts_with(&prefix) as i64))
        }
        "endsWith" => {
            let suffix = wasm_string_method_arg(runtime, memory, method, args, 0)?;
            Ok((TAG_BOOL, value.ends_with(&suffix) as i64))
        }
        "contains" => {
            let sub = wasm_string_method_arg(runtime, memory, method, args, 0)?;
            Ok((TAG_BOOL, value.contains(&sub) as i64))
        }
        "indexOf" => {
            let sub = wasm_string_method_arg(runtime, memory, method, args, 0)?;
            Ok(value
                .find(&sub)
                .map(|byte_index| (TAG_INT, value[..byte_index].chars().count() as i64))
                .unwrap_or((TAG_NULL, 0)))
        }
        "substring" => {
            expect_builtin_arg_count(method, args, 2)?;
            let start = wasm_int_method_arg(method, args, 0)?;
            let end = wasm_int_method_arg(method, args, 1)?;
            handle_data_result_to_wasm(
                runtime,
                HandleData::String(substring_chars_wasm(value, start, end)?),
            )
        }
        "replace" => {
            expect_builtin_arg_count(method, args, 2)?;
            let old = wasm_string_arg(runtime, memory, args[0])?;
            let new = wasm_string_arg(runtime, memory, args[1])?;
            handle_data_result_to_wasm(runtime, HandleData::String(value.replace(&old, &new)))
        }
        "split" => {
            let delim = wasm_string_method_arg(runtime, memory, method, args, 0)?;
            handle_data_result_to_wasm(
                runtime,
                HandleData::List(
                    value
                        .split(&delim)
                        .map(|part| HandleData::String(part.to_owned()))
                        .collect(),
                ),
            )
        }
        "toLower" => {
            expect_builtin_arg_count(method, args, 0)?;
            handle_data_result_to_wasm(runtime, HandleData::String(value.to_lowercase()))
        }
        "toUpper" => {
            expect_builtin_arg_count(method, args, 0)?;
            handle_data_result_to_wasm(runtime, HandleData::String(value.to_uppercase()))
        }
        "trim" => {
            expect_builtin_arg_count(method, args, 0)?;
            handle_data_result_to_wasm(runtime, HandleData::String(value.trim().to_owned()))
        }
        "repeat" => {
            let count = wasm_int_method_arg(method, args, 0)?;
            if count < 0 {
                return Err("repeat count must be non-negative".to_owned());
            }
            handle_data_result_to_wasm(runtime, HandleData::String(value.repeat(count as usize)))
        }
        _ => unknown_wasm_builtin(BuiltinReceiver::String, method),
    }
}

fn eval_list_builtin_wasm(
    runtime: &mut Runtime,
    memory: &[u8],
    items: &[HandleData],
    method: &str,
    args: &[(i32, i64)],
) -> Result<(i32, i64), String> {
    match method {
        "length" | "size" => {
            expect_builtin_arg_count(method, args, 0)?;
            Ok((TAG_INT, items.len() as i64))
        }
        "isEmpty" => {
            expect_builtin_arg_count(method, args, 0)?;
            Ok((TAG_BOOL, items.is_empty() as i64))
        }
        "first" => {
            expect_builtin_arg_count(method, args, 0)?;
            items
                .first()
                .cloned()
                .map(|item| handle_data_result_to_wasm(runtime, item))
                .unwrap_or(Ok((TAG_NULL, 0)))
        }
        "last" => {
            expect_builtin_arg_count(method, args, 0)?;
            items
                .last()
                .cloned()
                .map(|item| handle_data_result_to_wasm(runtime, item))
                .unwrap_or(Ok((TAG_NULL, 0)))
        }
        "get" => {
            let index = wasm_int_method_arg(method, args, 0)?;
            if index < 0 {
                return Ok((TAG_NULL, 0));
            }
            items
                .get(index as usize)
                .cloned()
                .map(|item| handle_data_result_to_wasm(runtime, item))
                .unwrap_or(Ok((TAG_NULL, 0)))
        }
        "contains" => {
            expect_builtin_arg_count(method, args, 1)?;
            let needle = wasm_to_data(runtime, Some(memory), args[0].0, args[0].1)?;
            Ok((TAG_BOOL, items.iter().any(|item| item == &needle) as i64))
        }
        "indexOf" => {
            expect_builtin_arg_count(method, args, 1)?;
            let needle = wasm_to_data(runtime, Some(memory), args[0].0, args[0].1)?;
            Ok(items
                .iter()
                .position(|item| item == &needle)
                .map(|index| (TAG_INT, index as i64))
                .unwrap_or((TAG_NULL, 0)))
        }
        "slice" => {
            expect_builtin_arg_count(method, args, 2)?;
            let from = wasm_int_method_arg(method, args, 0)?;
            let to = wasm_int_method_arg(method, args, 1)?;
            handle_data_result_to_wasm(runtime, HandleData::List(slice_data(items, from, to)?))
        }
        "reversed" => {
            expect_builtin_arg_count(method, args, 0)?;
            handle_data_result_to_wasm(
                runtime,
                HandleData::List(items.iter().cloned().rev().collect()),
            )
        }
        "fold" | "foldRight" | "map" | "filter" | "flatMap" | "zip" => Err(format!(
            "List.{method} is not supported by the WASM executor yet"
        )),
        _ => unknown_wasm_builtin(BuiltinReceiver::List, method),
    }
}

fn wasm_string_arg(runtime: &Runtime, memory: &[u8], arg: (i32, i64)) -> Result<String, String> {
    match wasm_to_data(runtime, Some(memory), arg.0, arg.1)? {
        HandleData::String(value) => Ok(value),
        other => Err(format!(
            "expected String, found {}",
            handle_data_type(&other)
        )),
    }
}

fn wasm_list_arg(
    runtime: &Runtime,
    memory: &[u8],
    arg: (i32, i64),
) -> Result<Vec<HandleData>, String> {
    match wasm_to_data(runtime, Some(memory), arg.0, arg.1)? {
        HandleData::List(items) => Ok(items),
        other => Err(format!("expected List, found {}", handle_data_type(&other))),
    }
}

fn wasm_string_method_arg(
    runtime: &Runtime,
    memory: &[u8],
    method: &str,
    args: &[(i32, i64)],
    index: usize,
) -> Result<String, String> {
    expect_builtin_arg_count(method, args, index + 1)?;
    wasm_string_arg(runtime, memory, args[index])
}

fn wasm_int_method_arg(method: &str, args: &[(i32, i64)], index: usize) -> Result<i64, String> {
    expect_builtin_arg_count(method, args, index + 1)?;
    if args[index].0 != TAG_INT {
        return Err(format!("`{method}` expected Int argument"));
    }
    Ok(args[index].1)
}

fn expect_builtin_arg_count(
    method: &str,
    args: &[(i32, i64)],
    expected: usize,
) -> Result<(), String> {
    if args.len() == expected {
        Ok(())
    } else {
        Err(format!(
            "`{method}` expects {expected} argument(s), got {}",
            args.len()
        ))
    }
}

fn substring_chars_wasm(value: &str, start: i64, end: i64) -> Result<String, String> {
    if start < 0 || end < start {
        return Err("substring bounds must satisfy 0 <= start <= end".to_owned());
    }
    let start = start as usize;
    let end = end as usize;
    let len = value.chars().count();
    if end > len {
        return Err("substring end is out of bounds".to_owned());
    }
    Ok(value.chars().skip(start).take(end - start).collect())
}

fn slice_data(items: &[HandleData], from: i64, to: i64) -> Result<Vec<HandleData>, String> {
    if from < 0 || to < from {
        return Err("slice bounds must satisfy 0 <= from <= to".to_owned());
    }
    let from = from as usize;
    let to = to as usize;
    if to > items.len() {
        return Err("slice end is out of bounds".to_owned());
    }
    Ok(items[from..to].to_vec())
}

fn float_to_int_wasm(value: f64) -> (i32, i64) {
    if !value.is_finite() || value < i64::MIN as f64 || value > i64::MAX as f64 {
        (TAG_NULL, 0)
    } else {
        (TAG_INT, value.trunc() as i64)
    }
}

fn checked_f64_to_i64_wasm(value: f64) -> Result<i64, String> {
    if !value.is_finite() || value < i64::MIN as f64 || value > i64::MAX as f64 {
        return Err("Float value is out of Int range".to_owned());
    }
    Ok(value as i64)
}

fn unknown_wasm_builtin(receiver: BuiltinReceiver, method: &str) -> Result<(i32, i64), String> {
    Err(format!(
        "unknown builtin method `{}.{method}`",
        receiver.name()
    ))
}

fn handle_data_type(data: &HandleData) -> &'static str {
    match data {
        HandleData::Null => "Null",
        HandleData::Bool(_) => "Bool",
        HandleData::Int(_) => "Int",
        HandleData::UInt(_) => "UInt",
        HandleData::Float(_) => "Float",
        HandleData::String(_) => "String",
        HandleData::List(_) => "List",
        HandleData::Tuple(_) => "Tuple",
        HandleData::Record(_) => "Record",
    }
}

fn builtin_project(
    runtime: &mut Runtime,
    memory: &[u8],
    args: &[(i32, i64)],
    extra: &[Vec<u8>],
) -> Result<(i32, i64), String> {
    if args.is_empty() || extra.is_empty() {
        return Err("Project missing args".to_owned());
    }
    let target = wasm_to_inline(runtime, Some(memory), args[0].0, args[0].1)?;
    let projection = parse_projection(&extra[0])?;
    inline_result_to_wasm(runtime, project_inline(target, &projection)?)
}

fn builtin_index(
    runtime: &mut Runtime,
    memory: &[u8],
    args: &[(i32, i64)],
) -> Result<(i32, i64), String> {
    if args.len() != 2 {
        return Err("Index expects target and index args".to_owned());
    }
    let index = wasm_to_inline(runtime, Some(memory), args[1].0, args[1].1)?;
    let target = wasm_to_data(runtime, Some(memory), args[0].0, args[0].1)?;
    handle_data_result_to_wasm(runtime, index_data(target, index)?)
}

fn builtin_updated(
    runtime: &mut Runtime,
    memory: &[u8],
    args: &[(i32, i64)],
    extra: &[Vec<u8>],
) -> Result<(i32, i64), String> {
    if args.len() != 2 || extra.is_empty() {
        return Err("Updated expects target, replacement, and path data".to_owned());
    }
    let target = wasm_to_data(runtime, Some(memory), args[0].0, args[0].1)?;
    let replacement = wasm_to_data(runtime, Some(memory), args[1].0, args[1].1)?;
    let path = parse_update_path(&extra[0])?;
    handle_data_result_to_wasm(runtime, update_data(target, &path, replacement)?)
}

fn builtin_type_test(
    runtime: &Runtime,
    memory: &[u8],
    args: &[(i32, i64)],
    extra: &[Vec<u8>],
) -> Result<(i32, i64), String> {
    if extra.is_empty() || args.is_empty() {
        return Err("TypeTest missing data".to_owned());
    }
    let ty = String::from_utf8(extra[0].clone()).unwrap_or_default();
    let result = if wasm_matches_type(runtime, Some(memory), args[0].0, args[0].1, &ty) {
        1i64
    } else {
        0i64
    };
    Ok((TAG_BOOL, result))
}

fn builtin_iterator(
    runtime: &mut Runtime,
    iterators: &mut BTreeMap<u64, WasmIteratorState>,
    memory: &[u8],
    args: &[(i32, i64)],
) -> Result<(i32, i64), String> {
    if args.is_empty() {
        return Err("Iterator missing argument".to_owned());
    }
    let iterable = wasm_to_data(runtime, Some(memory), args[0].0, args[0].1)?;
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
    iterators.insert(iterator_id, WasmIteratorState { items, position: 0 });
    Ok((TAG_HANDLE, iterator_id as i64))
}

fn builtin_iterator_next(
    runtime: &mut Runtime,
    iterators: &mut BTreeMap<u64, WasmIteratorState>,
    memory: &[u8],
    args: &[(i32, i64)],
) -> Result<(i32, i64), String> {
    if args.is_empty() {
        return Err("IteratorNext missing argument".to_owned());
    }
    let iterator_id = match wasm_to_inline(runtime, Some(memory), args[0].0, args[0].1)? {
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
    memory: &[u8],
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
    let start = wasm_to_inline(runtime, Some(memory), args[0].0, args[0].1)?;
    let end = if args.len() >= 2 {
        Some(wasm_to_inline(runtime, Some(memory), args[1].0, args[1].1)?)
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
            if let (HandleData::Int(start), HandleData::Int(end), HandleData::Bool(inclusive)) =
                (&items[0], &items[1], &items[2])
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

fn builtin_econ_new(
    runtime: &mut Runtime,
    memory: &[u8],
    args: &[(i32, i64)],
) -> Result<(i32, i64), String> {
    if args.is_empty() {
        return Err("Econ missing arg".to_owned());
    }
    let v = wasm_to_inline(runtime, Some(memory), args[0].0, args[0].1)?;
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
    memory: &[u8],
    args: &[(i32, i64)],
    extra: &[Vec<u8>],
) -> Result<(i32, i64), String> {
    if args.is_empty() {
        return Err("SafeProject missing arg".to_owned());
    }
    if args[0].0 == TAG_NULL {
        return Ok((TAG_NULL, 0));
    }
    builtin_project(runtime, memory, args, extra)
}

fn to_wasm_entry(
    runtime: &Runtime,
    memory: &Memory,
    heap_top: &Global,
    store: &mut Store<State>,
    value: &RuntimeValue,
) -> Result<(i32, i64), String> {
    match value {
        RuntimeValue::Inline(InlineValue::Int(value)) => Ok((TAG_INT, *value)),
        RuntimeValue::Inline(InlineValue::UInt(value)) => Ok((TAG_UINT, *value as i64)),
        RuntimeValue::Inline(InlineValue::Float(value)) => Ok((TAG_FLOAT, value.to_bits() as i64)),
        RuntimeValue::Inline(InlineValue::Bool(value)) => Ok((TAG_BOOL, *value as i64)),
        RuntimeValue::Inline(InlineValue::Null) => Ok((TAG_NULL, 0)),
        RuntimeValue::Inline(value) => {
            let data = handle_data_from_inline(value)?;
            write_handle_data_to_memory(memory, heap_top, store, &data)
        }
        RuntimeValue::Handle(handle) => match runtime.get_handle_data(*handle) {
            Ok(data) => write_handle_data_to_memory(memory, heap_top, store, &data),
            Err(_) => Ok(handle_to_wasm(runtime, *handle)),
        },
    }
}

fn to_wasm(runtime: &mut Runtime, value: &RuntimeValue) -> Result<(i32, i64), String> {
    match value {
        RuntimeValue::Inline(iv) => inline_result_to_wasm(runtime, iv.clone()),
        RuntimeValue::Handle(handle) => Ok(handle_to_wasm(runtime, *handle)),
    }
}

fn write_handle_data_to_memory(
    memory: &Memory,
    heap_top: &Global,
    store: &mut Store<State>,
    data: &HandleData,
) -> Result<(i32, i64), String> {
    match data {
        HandleData::Null => Ok((TAG_NULL, 0)),
        HandleData::Bool(value) => Ok((TAG_BOOL, *value as i64)),
        HandleData::Int(value) => Ok((TAG_INT, *value)),
        HandleData::UInt(value) => Ok((TAG_UINT, *value as i64)),
        HandleData::Float(value) => Ok((TAG_FLOAT, value.to_bits() as i64)),
        HandleData::String(value) => {
            let bytes = value.as_bytes();
            let offset = alloc_memory(memory, heap_top, store, 4 + bytes.len() as u32)?;
            memory
                .write(
                    &mut *store,
                    offset as usize,
                    &(bytes.len() as u32).to_le_bytes(),
                )
                .map_err(|error| format!("string length write failed: {error}"))?;
            memory
                .write(&mut *store, offset as usize + 4, bytes)
                .map_err(|error| format!("string bytes write failed: {error}"))?;
            Ok((TAG_STRING, offset as i64))
        }
        HandleData::Tuple(values) => {
            let items = values
                .iter()
                .map(|value| write_handle_data_to_memory(memory, heap_top, store, value))
                .collect::<Result<Vec<_>, _>>()?;
            write_sequence_to_memory(memory, heap_top, store, TAG_TUPLE, &items)
        }
        HandleData::List(values) => {
            let items = values
                .iter()
                .map(|value| write_handle_data_to_memory(memory, heap_top, store, value))
                .collect::<Result<Vec<_>, _>>()?;
            write_sequence_to_memory(memory, heap_top, store, TAG_LIST, &items)
        }
        HandleData::Record(fields) => {
            let items = fields
                .iter()
                .map(|(name, value)| {
                    Ok((
                        name.as_bytes().to_vec(),
                        write_handle_data_to_memory(memory, heap_top, store, value)?,
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?;
            let mut size = 4u32;
            for (name, _) in &items {
                size = size
                    .checked_add(4)
                    .and_then(|v| v.checked_add(name.len() as u32))
                    .and_then(|v| v.checked_add(12))
                    .ok_or_else(|| "record allocation size overflow".to_owned())?;
            }
            let offset = alloc_memory(memory, heap_top, store, size)?;
            memory
                .write(
                    &mut *store,
                    offset as usize,
                    &(items.len() as u32).to_le_bytes(),
                )
                .map_err(|error| format!("record field count write failed: {error}"))?;
            let mut pos = offset + 4;
            for (name, (tag, val)) in items {
                memory
                    .write(
                        &mut *store,
                        pos as usize,
                        &(name.len() as u32).to_le_bytes(),
                    )
                    .map_err(|error| format!("record field name length write failed: {error}"))?;
                pos += 4;
                memory
                    .write(&mut *store, pos as usize, &name)
                    .map_err(|error| format!("record field name write failed: {error}"))?;
                pos += name.len() as u32;
                memory
                    .write(&mut *store, pos as usize, &tag.to_le_bytes())
                    .map_err(|error| format!("record field tag write failed: {error}"))?;
                pos += 4;
                memory
                    .write(&mut *store, pos as usize, &val.to_le_bytes())
                    .map_err(|error| format!("record field data write failed: {error}"))?;
                pos += 8;
            }
            Ok((TAG_RECORD, offset as i64))
        }
    }
}

fn write_sequence_to_memory(
    memory: &Memory,
    heap_top: &Global,
    store: &mut Store<State>,
    tag: i32,
    items: &[(i32, i64)],
) -> Result<(i32, i64), String> {
    let size = 4u32
        .checked_add(
            (items.len() as u32)
                .checked_mul(16)
                .ok_or_else(|| "sequence allocation size overflow".to_owned())?,
        )
        .ok_or_else(|| "sequence allocation size overflow".to_owned())?;
    let offset = alloc_memory(memory, heap_top, store, size)?;
    memory
        .write(
            &mut *store,
            offset as usize,
            &(items.len() as u32).to_le_bytes(),
        )
        .map_err(|error| format!("sequence count write failed: {error}"))?;
    for (i, (item_tag, item_val)) in items.iter().enumerate() {
        let base = offset as usize + 4 + i * 16;
        memory
            .write(&mut *store, base, &item_tag.to_le_bytes())
            .map_err(|error| format!("sequence tag write failed: {error}"))?;
        memory
            .write(&mut *store, base + 8, &item_val.to_le_bytes())
            .map_err(|error| format!("sequence data write failed: {error}"))?;
    }
    Ok((tag, offset as i64))
}

fn alloc_memory(
    memory: &Memory,
    heap_top: &Global,
    store: &mut Store<State>,
    size: u32,
) -> Result<u32, String> {
    let size = align_to(size, 8);
    let current: u32 = heap_top
        .get(&mut *store)
        .unwrap_i32()
        .try_into()
        .map_err(|_| "heap_top is negative".to_owned())?;
    let next = current
        .checked_add(size)
        .ok_or_else(|| "wasm heap allocation overflow".to_owned())?;
    if next > HEAP_LIMIT {
        return Err(format!(
            "wasm heap exhausted: requested {size} bytes with heap_top {current}"
        ));
    }
    let memory_len = memory.data_size(&mut *store) as u32;
    if next > memory_len {
        return Err(format!(
            "wasm heap allocation exceeds memory size: requested end {next}, memory size {memory_len}"
        ));
    }
    heap_top
        .set(&mut *store, Val::I32(next as i32))
        .map_err(|error| format!("heap_top update failed: {error}"))?;
    Ok(current)
}

fn align_to(value: u32, align: u32) -> u32 {
    debug_assert!(align.is_power_of_two());
    (value + align - 1) & !(align - 1)
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

fn memory_backed_offset(val: i64) -> bool {
    val >= STRDATA_OFF
}

fn memory_value_to_handle_data(data: &[u8], tag: i32, val: i64) -> Result<HandleData, String> {
    memory_value_to_handle_data_depth(data, tag, val, 0)
}

fn memory_value_to_handle_data_depth(
    data: &[u8],
    tag: i32,
    val: i64,
    depth: usize,
) -> Result<HandleData, String> {
    if depth > MAX_MEMORY_VALUE_DEPTH {
        return Err("wasm memory value nesting is too deep".to_owned());
    }
    let offset =
        u32::try_from(val).map_err(|_| format!("wasm memory offset {val} is not a valid u32"))?;
    match tag {
        TAG_STRING => {
            let len = mem_read_i32(data, offset)? as u32;
            let start = offset
                .checked_add(4)
                .ok_or_else(|| "string offset overflow".to_owned())?;
            let end = start
                .checked_add(len)
                .ok_or_else(|| "string length overflow".to_owned())?;
            let bytes = data
                .get(start as usize..end as usize)
                .ok_or_else(|| "string data out of bounds".to_owned())?;
            let text = String::from_utf8(bytes.to_vec())
                .map_err(|error| format!("invalid utf8: {error}"))?;
            Ok(HandleData::String(text))
        }
        TAG_TUPLE | TAG_LIST => {
            let count = mem_read_i32(data, offset)? as u32;
            let mut values = Vec::with_capacity(count as usize);
            for i in 0..count {
                let base = offset
                    .checked_add(4)
                    .and_then(|v| v.checked_add(i.checked_mul(16)?))
                    .ok_or_else(|| "sequence offset overflow".to_owned())?;
                let item_tag = mem_read_i32(data, base)?;
                let item_data = mem_read_i64(data, base + 8)?;
                values.push(memory_or_inline_to_handle_data(
                    data,
                    item_tag,
                    item_data,
                    depth + 1,
                )?);
            }
            if tag == TAG_TUPLE {
                Ok(HandleData::Tuple(values))
            } else {
                Ok(HandleData::List(values))
            }
        }
        TAG_RECORD => {
            let count = mem_read_i32(data, offset)? as u32;
            let mut fields = BTreeMap::new();
            let mut pos = offset
                .checked_add(4)
                .ok_or_else(|| "record offset overflow".to_owned())?;
            for _ in 0..count {
                let name_len = mem_read_i32(data, pos)? as u32;
                pos = pos
                    .checked_add(4)
                    .ok_or_else(|| "record name offset overflow".to_owned())?;
                let name_end = pos
                    .checked_add(name_len)
                    .ok_or_else(|| "record name length overflow".to_owned())?;
                let name_bytes = data
                    .get(pos as usize..name_end as usize)
                    .ok_or_else(|| "record field name out of bounds".to_owned())?;
                let name = String::from_utf8(name_bytes.to_vec())
                    .map_err(|error| format!("invalid record field name: {error}"))?;
                pos = name_end;
                let field_tag = mem_read_i32(data, pos)?;
                pos = pos
                    .checked_add(4)
                    .ok_or_else(|| "record field tag offset overflow".to_owned())?;
                let field_data = mem_read_i64(data, pos)?;
                pos = pos
                    .checked_add(8)
                    .ok_or_else(|| "record field data offset overflow".to_owned())?;
                fields.insert(
                    name,
                    memory_or_inline_to_handle_data(data, field_tag, field_data, depth + 1)?,
                );
            }
            Ok(HandleData::Record(fields))
        }
        _ => Err(format!("tag {tag} is not a memory-backed complex value")),
    }
}

fn memory_or_inline_to_handle_data(
    data: &[u8],
    tag: i32,
    val: i64,
    depth: usize,
) -> Result<HandleData, String> {
    match tag {
        TAG_INT => Ok(HandleData::Int(val)),
        TAG_UINT => Ok(HandleData::UInt(val as u64)),
        TAG_FLOAT => Ok(HandleData::Float(f64::from_bits(val as u64))),
        TAG_BOOL => Ok(HandleData::Bool(val != 0)),
        TAG_NULL => Ok(HandleData::Null),
        TAG_TUPLE if val == 0 => Ok(HandleData::Tuple(vec![])),
        TAG_STRING | TAG_TUPLE | TAG_RECORD | TAG_LIST if memory_backed_offset(val) => {
            memory_value_to_handle_data_depth(data, tag, val, depth)
        }
        other => Err(format!(
            "memory-backed value contains unsupported nested tag {other}"
        )),
    }
}

fn handle_data_to_runtime_handle(
    runtime: &mut Runtime,
    data: HandleData,
) -> Result<RuntimeValue, String> {
    let (tag, val) = handle_data_result_to_wasm(runtime, data)?;
    match tag {
        TAG_STRING | TAG_TUPLE | TAG_RECORD => {
            let handle = HandleId(val as u64);
            match runtime.get_handle_data(handle) {
                Ok(data) => handle_data_to_inline(data).map(RuntimeValue::Inline),
                Err(_) => Ok(RuntimeValue::Handle(handle)),
            }
        }
        TAG_LIST | TAG_HANDLE => Ok(RuntimeValue::Handle(HandleId(val as u64))),
        TAG_INT | TAG_UINT | TAG_FLOAT | TAG_BOOL | TAG_NULL => from_wasm(runtime, None, tag, val),
        _ => Err(format!("unknown wasm result tag {tag}")),
    }
}

fn from_wasm(
    runtime: &mut Runtime,
    memory: Option<&[u8]>,
    tag: i32,
    val: i64,
) -> Result<RuntimeValue, String> {
    match tag {
        TAG_INT => Ok(RuntimeValue::Inline(InlineValue::Int(val))),
        TAG_UINT => Ok(RuntimeValue::Inline(InlineValue::UInt(val as u64))),
        TAG_FLOAT => Ok(RuntimeValue::Inline(InlineValue::Float(f64::from_bits(
            val as u64,
        )))),
        TAG_BOOL => Ok(RuntimeValue::Inline(InlineValue::Bool(val != 0))),
        TAG_NULL => Ok(RuntimeValue::Inline(InlineValue::Null)),
        TAG_TUPLE if val == 0 => Ok(RuntimeValue::Inline(InlineValue::Tuple(vec![]))),
        TAG_STRING | TAG_TUPLE | TAG_RECORD | TAG_LIST
            if memory_backed_offset(val)
                && memory
                    .map(|data| memory_value_to_handle_data(data, tag, val).is_ok())
                    .unwrap_or(false) =>
        {
            let data = memory_value_to_handle_data(memory.expect("checked above"), tag, val)?;
            match data {
                HandleData::List(_) => handle_data_to_runtime_handle(runtime, data),
                other => handle_data_to_inline(other).map(RuntimeValue::Inline),
            }
        }
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

fn wasm_to_inline(
    runtime: &Runtime,
    memory: Option<&[u8]>,
    tag: i32,
    val: i64,
) -> Result<InlineValue, String> {
    match tag {
        TAG_INT => Ok(InlineValue::Int(val)),
        TAG_UINT => Ok(InlineValue::UInt(val as u64)),
        TAG_FLOAT => Ok(InlineValue::Float(f64::from_bits(val as u64))),
        TAG_BOOL => Ok(InlineValue::Bool(val != 0)),
        TAG_NULL => Ok(InlineValue::Null),
        TAG_TUPLE if val == 0 => Ok(InlineValue::Tuple(vec![])),
        TAG_STRING | TAG_TUPLE | TAG_RECORD
            if memory_backed_offset(val)
                && memory
                    .map(|data| memory_value_to_handle_data(data, tag, val).is_ok())
                    .unwrap_or(false) =>
        {
            memory_value_to_handle_data(memory.expect("checked above"), tag, val)
                .and_then(handle_data_to_inline)
        }
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
        InlineValue::UInt(value) => Ok((TAG_UINT, value as i64)),
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

fn wasm_to_data(
    runtime: &Runtime,
    memory: Option<&[u8]>,
    tag: i32,
    val: i64,
) -> Result<HandleData, String> {
    match tag {
        TAG_INT => Ok(HandleData::Int(val)),
        TAG_UINT => Ok(HandleData::UInt(val as u64)),
        TAG_FLOAT => Ok(HandleData::Float(f64::from_bits(val as u64))),
        TAG_BOOL => Ok(HandleData::Bool(val != 0)),
        TAG_NULL => Ok(HandleData::Null),
        TAG_STRING | TAG_TUPLE | TAG_RECORD | TAG_LIST
            if memory_backed_offset(val)
                && memory
                    .map(|data| memory_value_to_handle_data(data, tag, val).is_ok())
                    .unwrap_or(false) =>
        {
            memory_value_to_handle_data(memory.expect("checked above"), tag, val)
        }
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
        HandleData::UInt(value) => Ok((TAG_UINT, value as i64)),
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
        HandleData::UInt(value) => Ok(InlineValue::UInt(value)),
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
        InlineValue::UInt(value) => Ok(HandleData::UInt(*value)),
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

fn wasm_matches_type(
    runtime: &Runtime,
    memory: Option<&[u8]>,
    tag: i32,
    val: i64,
    ty: &str,
) -> bool {
    match (ty, tag) {
        ("Int", TAG_INT)
        | ("UInt", TAG_UINT)
        | ("Float", TAG_FLOAT)
        | ("Bool", TAG_BOOL)
        | ("String", TAG_STRING)
        | ("Null", TAG_NULL)
        | ("Tuple", TAG_TUPLE)
        | ("Record", TAG_RECORD)
        | ("List", TAG_LIST) => return true,
        ("Unit", TAG_TUPLE) => {
            if let Some(data) = memory.filter(|_| memory_backed_offset(val)) {
                return matches!(
                    memory_value_to_handle_data(data, tag, val),
                    Ok(HandleData::Tuple(items)) if items.is_empty()
                );
            }
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
        "UInt" => Some(TAG_UINT),
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
        InlineValue::UInt(_) => "UInt",
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
        HandleData::UInt(_) => "UInt",
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
        HandleData::UInt(value) => value.to_string(),
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
        InlineValue::UInt(v) => v.to_string(),
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
