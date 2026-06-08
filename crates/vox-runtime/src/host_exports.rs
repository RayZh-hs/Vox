use std::{collections::BTreeMap, sync::Arc};

use vox_core::{
    host::PackageManifest,
    ids::LibraryId,
    value::{HandleData, HandleSummary, InlineValue, RuntimeValue},
};

use crate::{HostCallArgument, HostFunctionHandler, Runtime};

pub use vox_core::external_export::inventory;

pub type HostExportInvoker = fn(&mut Runtime, &[HostCallArgument]) -> Result<RuntimeValue, String>;

#[derive(Debug, Clone, Copy)]
pub struct RegisteredHostFunctionImplementation {
    pub rust_name: &'static str,
    pub vox_name: &'static str,
    pub invoke: HostExportInvoker,
    pub order: u32,
}

inventory::collect!(RegisteredHostFunctionImplementation);

pub fn mount_registered_host_library(
    runtime: &mut Runtime,
    manifest: PackageManifest,
) -> Result<LibraryId, String> {
    let handlers = registered_host_functions_for_manifest(&manifest)?;
    runtime.mount_host_library(manifest, handlers)
}

pub fn required_argument<'a>(
    arguments: &'a [HostCallArgument],
    index: usize,
    name: &str,
) -> Result<&'a RuntimeValue, String> {
    let argument = arguments
        .get(index)
        .ok_or_else(|| format!("missing host argument `{name}`"))?;
    argument
        .value
        .as_ref()
        .ok_or_else(|| format!("host argument `{name}` must be provided"))
}

pub trait FromHostValue: Sized {
    fn from_host_value(runtime: &Runtime, value: &RuntimeValue) -> Result<Self, String>;
}

pub trait IntoHostValue {
    fn into_host_value(self, runtime: &mut Runtime) -> Result<RuntimeValue, String>;
}

pub trait FromVoxFieldData: Sized {
    fn from_vox_field_data(data: HandleData) -> Result<Self, String>;
}

pub trait IntoVoxFieldData {
    fn into_vox_field_data(self) -> Result<HandleData, String>;
}

pub trait VoxHandleValue: Sized {
    fn vox_type_name() -> &'static str;

    fn from_vox_handle_data(data: HandleData) -> Result<Self, String>;

    fn into_vox_handle_data(self) -> Result<HandleData, String>;

    fn vox_handle_summary(&self) -> String {
        Self::vox_type_name().to_owned()
    }
}

impl FromHostValue for String {
    fn from_host_value(_: &Runtime, value: &RuntimeValue) -> Result<Self, String> {
        match value {
            RuntimeValue::Inline(InlineValue::String(value)) => Ok(value.clone()),
            other => Err(format!(
                "expected String host value, received {}",
                runtime_value_kind(other)
            )),
        }
    }
}

impl IntoHostValue for String {
    fn into_host_value(self, _: &mut Runtime) -> Result<RuntimeValue, String> {
        Ok(RuntimeValue::Inline(InlineValue::String(self)))
    }
}

impl FromHostValue for bool {
    fn from_host_value(_: &Runtime, value: &RuntimeValue) -> Result<Self, String> {
        match value {
            RuntimeValue::Inline(InlineValue::Bool(value)) => Ok(*value),
            other => Err(format!(
                "expected Bool host value, received {}",
                runtime_value_kind(other)
            )),
        }
    }
}

impl IntoHostValue for bool {
    fn into_host_value(self, _: &mut Runtime) -> Result<RuntimeValue, String> {
        Ok(RuntimeValue::Inline(InlineValue::Bool(self)))
    }
}

impl FromHostValue for i64 {
    fn from_host_value(_: &Runtime, value: &RuntimeValue) -> Result<Self, String> {
        match value {
            RuntimeValue::Inline(InlineValue::Int(value)) => Ok(*value),
            other => Err(format!(
                "expected Int host value, received {}",
                runtime_value_kind(other)
            )),
        }
    }
}

impl IntoHostValue for i64 {
    fn into_host_value(self, _: &mut Runtime) -> Result<RuntimeValue, String> {
        Ok(RuntimeValue::Inline(InlineValue::Int(self)))
    }
}

impl FromHostValue for f64 {
    fn from_host_value(_: &Runtime, value: &RuntimeValue) -> Result<Self, String> {
        match value {
            RuntimeValue::Inline(InlineValue::Float(value)) => Ok(*value),
            RuntimeValue::Inline(InlineValue::Int(value)) => Ok(*value as f64),
            other => Err(format!(
                "expected Float host value, received {}",
                runtime_value_kind(other)
            )),
        }
    }
}

impl IntoHostValue for f64 {
    fn into_host_value(self, _: &mut Runtime) -> Result<RuntimeValue, String> {
        Ok(RuntimeValue::Inline(InlineValue::Float(self)))
    }
}

impl IntoHostValue for () {
    fn into_host_value(self, _: &mut Runtime) -> Result<RuntimeValue, String> {
        Ok(RuntimeValue::Inline(InlineValue::Tuple(Vec::new())))
    }
}

impl<T> FromHostValue for Option<T>
where
    T: FromHostValue,
{
    fn from_host_value(runtime: &Runtime, value: &RuntimeValue) -> Result<Self, String> {
        if matches!(value, RuntimeValue::Inline(InlineValue::Null)) {
            return Ok(None);
        }
        T::from_host_value(runtime, value).map(Some)
    }
}

impl<T> IntoHostValue for Option<T>
where
    T: IntoHostValue,
{
    fn into_host_value(self, runtime: &mut Runtime) -> Result<RuntimeValue, String> {
        match self {
            Some(value) => value.into_host_value(runtime),
            None => Ok(RuntimeValue::Inline(InlineValue::Null)),
        }
    }
}

impl<T> FromHostValue for Vec<T>
where
    T: FromVoxFieldData,
{
    fn from_host_value(runtime: &Runtime, value: &RuntimeValue) -> Result<Self, String> {
        match value {
            RuntimeValue::Handle(handle) => {
                let data = runtime.get_handle_data(*handle)?;
                <Self as FromVoxFieldData>::from_vox_field_data(data)
            }
            other => Err(format!(
                "expected List host value, received {}",
                runtime_value_kind(other)
            )),
        }
    }
}

impl<T> IntoHostValue for Vec<T>
where
    T: IntoVoxFieldData,
{
    fn into_host_value(self, runtime: &mut Runtime) -> Result<RuntimeValue, String> {
        let summary = HandleSummary {
            type_name: "List".to_owned(),
            summary: "List".to_owned(),
            bytes: None,
        };
        let data = <Self as IntoVoxFieldData>::into_vox_field_data(self)?;
        Ok(RuntimeValue::Handle(
            runtime.allocate_serializable_handle(summary, data),
        ))
    }
}

impl<T> FromHostValue for T
where
    T: VoxHandleValue,
{
    fn from_host_value(runtime: &Runtime, value: &RuntimeValue) -> Result<Self, String> {
        match value {
            RuntimeValue::Handle(handle) => {
                let data = runtime.get_handle_data(*handle)?;
                T::from_vox_handle_data(data)
            }
            other => Err(format!(
                "expected {} host value, received {}",
                T::vox_type_name(),
                runtime_value_kind(other)
            )),
        }
    }
}

impl<T> IntoHostValue for T
where
    T: VoxHandleValue,
{
    fn into_host_value(self, runtime: &mut Runtime) -> Result<RuntimeValue, String> {
        let summary = HandleSummary {
            type_name: T::vox_type_name().to_owned(),
            summary: self.vox_handle_summary(),
            bytes: None,
        };
        let data = self.into_vox_handle_data()?;
        Ok(RuntimeValue::Handle(
            runtime.allocate_serializable_handle(summary, data),
        ))
    }
}

impl FromVoxFieldData for bool {
    fn from_vox_field_data(data: HandleData) -> Result<Self, String> {
        match data {
            HandleData::Bool(value) => Ok(value),
            other => Err(format!(
                "expected Bool handle data, received {}",
                handle_data_kind(&other)
            )),
        }
    }
}

impl IntoVoxFieldData for bool {
    fn into_vox_field_data(self) -> Result<HandleData, String> {
        Ok(HandleData::Bool(self))
    }
}

impl FromVoxFieldData for i64 {
    fn from_vox_field_data(data: HandleData) -> Result<Self, String> {
        match data {
            HandleData::Int(value) => Ok(value),
            other => Err(format!(
                "expected Int handle data, received {}",
                handle_data_kind(&other)
            )),
        }
    }
}

impl IntoVoxFieldData for i64 {
    fn into_vox_field_data(self) -> Result<HandleData, String> {
        Ok(HandleData::Int(self))
    }
}

impl FromVoxFieldData for u8 {
    fn from_vox_field_data(data: HandleData) -> Result<Self, String> {
        let value = <i64 as FromVoxFieldData>::from_vox_field_data(data)?;
        u8::try_from(value).map_err(|_| format!("integer value {value} is outside the u8 range"))
    }
}

impl IntoVoxFieldData for u8 {
    fn into_vox_field_data(self) -> Result<HandleData, String> {
        Ok(HandleData::Int(self as i64))
    }
}

impl FromVoxFieldData for f64 {
    fn from_vox_field_data(data: HandleData) -> Result<Self, String> {
        match data {
            HandleData::Float(value) => Ok(value),
            HandleData::Int(value) => Ok(value as f64),
            other => Err(format!(
                "expected Float handle data, received {}",
                handle_data_kind(&other)
            )),
        }
    }
}

impl IntoVoxFieldData for f64 {
    fn into_vox_field_data(self) -> Result<HandleData, String> {
        Ok(HandleData::Float(self))
    }
}

impl FromVoxFieldData for String {
    fn from_vox_field_data(data: HandleData) -> Result<Self, String> {
        match data {
            HandleData::String(value) => Ok(value),
            other => Err(format!(
                "expected String handle data, received {}",
                handle_data_kind(&other)
            )),
        }
    }
}

impl IntoVoxFieldData for String {
    fn into_vox_field_data(self) -> Result<HandleData, String> {
        Ok(HandleData::String(self))
    }
}

impl<T> FromVoxFieldData for Option<T>
where
    T: FromVoxFieldData,
{
    fn from_vox_field_data(data: HandleData) -> Result<Self, String> {
        if matches!(data, HandleData::Null) {
            return Ok(None);
        }
        T::from_vox_field_data(data).map(Some)
    }
}

impl<T> IntoVoxFieldData for Option<T>
where
    T: IntoVoxFieldData,
{
    fn into_vox_field_data(self) -> Result<HandleData, String> {
        match self {
            Some(value) => value.into_vox_field_data(),
            None => Ok(HandleData::Null),
        }
    }
}

impl<T> FromVoxFieldData for Vec<T>
where
    T: FromVoxFieldData,
{
    fn from_vox_field_data(data: HandleData) -> Result<Self, String> {
        match data {
            HandleData::List(values) => values.into_iter().map(T::from_vox_field_data).collect(),
            other => Err(format!(
                "expected List handle data, received {}",
                handle_data_kind(&other)
            )),
        }
    }
}

impl<T> IntoVoxFieldData for Vec<T>
where
    T: IntoVoxFieldData,
{
    fn into_vox_field_data(self) -> Result<HandleData, String> {
        Ok(HandleData::List(
            self.into_iter()
                .map(T::into_vox_field_data)
                .collect::<Result<Vec<_>, _>>()?,
        ))
    }
}

impl<T> FromVoxFieldData for T
where
    T: VoxHandleValue,
{
    fn from_vox_field_data(data: HandleData) -> Result<Self, String> {
        T::from_vox_handle_data(data)
    }
}

impl<T> IntoVoxFieldData for T
where
    T: VoxHandleValue,
{
    fn into_vox_field_data(self) -> Result<HandleData, String> {
        self.into_vox_handle_data()
    }
}

fn registered_host_functions_for_manifest(
    manifest: &PackageManifest,
) -> Result<Vec<(String, HostFunctionHandler)>, String> {
    let mut registered = inventory::iter::<RegisteredHostFunctionImplementation>
        .into_iter()
        .copied()
        .collect::<Vec<_>>();
    registered.sort_by(|left, right| {
        left.order
            .cmp(&right.order)
            .then(left.vox_name.cmp(right.vox_name))
            .then(left.rust_name.cmp(right.rust_name))
    });

    let mut registry = BTreeMap::new();
    for function in registered {
        if let Some(previous) = registry.insert(function.vox_name, function) {
            return Err(format!(
                "duplicate exported host implementation `{}` from `{}` and `{}`",
                function.vox_name, previous.rust_name, function.rust_name
            ));
        }
    }

    manifest
        .functions
        .iter()
        .map(|function| {
            let registration = registry.get(function.name.as_str()).ok_or_else(|| {
                format!(
                    "host function `{}` does not have an exported Rust implementation",
                    function.name
                )
            })?;
            let invoke = registration.invoke;
            Ok((
                function.name.clone(),
                Arc::new(
                    move |runtime: &mut Runtime, arguments: &[HostCallArgument]| {
                        invoke(runtime, arguments)
                    },
                ) as HostFunctionHandler,
            ))
        })
        .collect()
}

fn runtime_value_kind(value: &RuntimeValue) -> &'static str {
    match value {
        RuntimeValue::Inline(InlineValue::Int(_)) => "Int",
        RuntimeValue::Inline(InlineValue::Float(_)) => "Float",
        RuntimeValue::Inline(InlineValue::Bool(_)) => "Bool",
        RuntimeValue::Inline(InlineValue::String(_)) => "String",
        RuntimeValue::Inline(InlineValue::Tuple(_)) => "Tuple",
        RuntimeValue::Inline(InlineValue::Record(_)) => "Record",
        RuntimeValue::Inline(InlineValue::Handle(_)) => "Handle",
        RuntimeValue::Inline(InlineValue::Null) => "Null",
        RuntimeValue::Handle(_) => "Handle",
    }
}

fn handle_data_kind(value: &HandleData) -> &'static str {
    match value {
        HandleData::Int(_) => "Int",
        HandleData::Float(_) => "Float",
        HandleData::Bool(_) => "Bool",
        HandleData::String(_) => "String",
        HandleData::List(_) => "List",
        HandleData::Tuple(_) => "Tuple",
        HandleData::Record(_) => "Record",
        HandleData::Null => "Null",
    }
}
