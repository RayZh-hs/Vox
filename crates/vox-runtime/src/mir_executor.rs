use std::collections::BTreeMap;

use vox_core::{
    host::Purity,
    ids::ArtifactId,
    mir::{
        MirBlock, MirBlockId, MirBody, MirBodyKind, MirModule, MirOp, MirOpKind, MirPathSegment,
        MirProjection, MirTerminator, MirValueDefinition, MirValueId, MirVersionId,
    },
    plan::CompiledArtifact,
    source::ModulePath,
    value::{HandleData, HandleSummary, InlineValue, RuntimeValue},
};

use crate::{HostCallArgument, Runtime};

pub struct MirExecutor<'a> {
    runtime: &'a mut Runtime,
    artifact_id: ArtifactId,
    module: &'a MirModule,
    values: BTreeMap<MirValueId, InlineValue>,
    versions: BTreeMap<MirVersionId, InlineValue>,
}

impl<'a> MirExecutor<'a> {
    pub fn new(runtime: &'a mut Runtime, artifact_id: ArtifactId, module: &'a MirModule) -> Self {
        Self {
            runtime,
            artifact_id,
            module,
            values: BTreeMap::new(),
            versions: BTreeMap::new(),
        }
    }

    pub fn run_script(
        &mut self,
        artifact: &CompiledArtifact,
        arguments: &[RuntimeValue],
    ) -> Result<RuntimeValue, String> {
        let body = self
            .module
            .bodies
            .iter()
            .find(|body| matches!(body.kind, MirBodyKind::ScriptEntry))
            .ok_or_else(|| "MIR module does not contain a script entry body".to_owned())?;
        self.run_body(body, artifact, arguments)
            .map(runtime_value_from_inline)
    }

    fn run_body(
        &mut self,
        body: &MirBody,
        artifact: &CompiledArtifact,
        arguments: &[RuntimeValue],
    ) -> Result<InlineValue, String> {
        self.values.clear();
        self.versions.clear();

        if arguments.len() > body.parameters.len() {
            return Err(format!(
                "script expects at most {} argument(s), but received {}",
                body.parameters.len(),
                arguments.len()
            ));
        }
        if arguments.len() < body.parameters.len()
            && artifact
                .parameters
                .iter()
                .skip(arguments.len())
                .any(|parameter| parameter.has_default)
        {
            return Err("MIR execution does not evaluate script parameter defaults yet".to_owned());
        }
        if arguments.len() < body.parameters.len() {
            let missing = artifact
                .parameters
                .get(arguments.len())
                .map(|parameter| parameter.name.as_str())
                .unwrap_or("<unknown>");
            return Err(format!("missing required script parameter `{missing}`"));
        }

        for (value, argument) in body.parameters.iter().zip(arguments) {
            self.values
                .insert(*value, inline_value_from_runtime(argument));
        }

        let mut current = body
            .blocks
            .first()
            .map(|block| block.id)
            .ok_or_else(|| "MIR body does not contain an entry block".to_owned())?;
        let mut fuel = 100_000_u32;
        loop {
            if fuel == 0 {
                return Err("MIR execution exceeded the control-flow step limit".to_owned());
            }
            fuel -= 1;

            let block = block_by_id(body, current)?;
            for op in &block.ops {
                self.eval_op(body, op)?;
            }

            match &block.terminator {
                MirTerminator::Jump { target, args } => {
                    self.bind_block_args(body, *target, args)?;
                    current = *target;
                }
                MirTerminator::Branch {
                    condition,
                    then_target,
                    then_args,
                    else_target,
                    else_args,
                } => {
                    let condition = expect_bool(self.value(*condition)?, "branch condition")?;
                    if condition {
                        self.bind_block_args(body, *then_target, then_args)?;
                        current = *then_target;
                    } else {
                        self.bind_block_args(body, *else_target, else_args)?;
                        current = *else_target;
                    }
                }
                MirTerminator::Return(value) => return self.value(*value),
                MirTerminator::Panic(message) => return Err(format!("panic: {message}")),
                MirTerminator::Unreachable => {
                    return Err(format!(
                        "MIR reached unreachable block %bb{} in body @{}",
                        block.id.0, body.name
                    ));
                }
            }
        }
    }

    fn bind_block_args(
        &mut self,
        body: &MirBody,
        target: MirBlockId,
        args: &[MirValueId],
    ) -> Result<(), String> {
        let block = block_by_id(body, target)?;
        if block.parameters.len() != args.len() {
            return Err(format!(
                "block %bb{} expects {} argument(s), received {}",
                target.0,
                block.parameters.len(),
                args.len()
            ));
        }

        let values = args
            .iter()
            .map(|arg| self.value(*arg))
            .collect::<Result<Vec<_>, _>>()?;
        for (parameter, value) in block.parameters.iter().zip(values) {
            self.values.insert(*parameter, value);
        }
        Ok(())
    }

    fn eval_op(&mut self, body: &MirBody, op: &MirOp) -> Result<(), String> {
        let result = match &op.kind {
            MirOpKind::Literal(value) => Some(value.clone()),
            MirOpKind::Unit => Some(InlineValue::Tuple(Vec::new())),
            MirOpKind::Use(version) => Some(self.version(*version)?),
            MirOpKind::Bind(version) => {
                let value = op
                    .args
                    .first()
                    .copied()
                    .ok_or_else(|| "bind op missing source value".to_owned())
                    .and_then(|value| self.value(value))?;
                self.versions.insert(*version, value);
                None
            }
            MirOpKind::Unary(name) => {
                let value = self.single_arg(op)?;
                Some(eval_unary(name, value)?)
            }
            MirOpKind::Binary(name) => {
                let (left, right) = self.two_args(op)?;
                Some(eval_binary(name, left, right)?)
            }
            MirOpKind::Tuple { .. } => Some(InlineValue::Tuple(self.arg_values(op)?)),
            MirOpKind::Record { fields } => {
                let values = self.arg_values(op)?;
                if fields.len() != values.len() {
                    return Err("record op field count does not match argument count".to_owned());
                }
                Some(InlineValue::Record(
                    fields.iter().cloned().zip(values).collect(),
                ))
            }
            MirOpKind::List => Some(self.materialize_list(self.arg_values(op)?)),
            MirOpKind::StringInterpolate { text } => {
                Some(eval_string_interpolation(text, self.arg_values(op)?)?)
            }
            MirOpKind::Project(projection) => {
                let value = self.single_arg(op)?;
                Some(project_value(value, projection)?)
            }
            MirOpKind::Index => {
                let (target, index) = self.two_args(op)?;
                Some(index_value(target, index)?)
            }
            MirOpKind::Updated { path } => {
                let [target, replacement] = op.args.as_slice() else {
                    return Err("updated op expects target and replacement values".to_owned());
                };
                let target = self.value(*target)?;
                let replacement = self.value(*replacement)?;
                Some(apply_updated_path(target, path, replacement)?)
            }
            MirOpKind::Call { callee, purity } => {
                Some(self.eval_call(body, callee, *purity, &op.args)?)
            }
            MirOpKind::Econ { .. } => Some(self.materialize_econ(self.single_arg(op)?)),
            MirOpKind::NonNull => {
                let value = self.single_arg(op)?;
                if matches!(value, InlineValue::Null) {
                    return Err("cannot unwrap null value".to_owned());
                }
                Some(value)
            }
            MirOpKind::SafeProject(field) => {
                let value = self.single_arg(op)?;
                if matches!(value, InlineValue::Null) {
                    Some(InlineValue::Null)
                } else {
                    Some(project_value(value, &MirProjection::Field(field.clone()))?)
                }
            }
            MirOpKind::TypeTest(ty) => {
                let value = self.single_arg(op)?;
                Some(InlineValue::Bool(matches_type_name(&value, ty)))
            }
            MirOpKind::TypeRefine(_) => Some(self.single_arg(op)?),
            MirOpKind::CacheGet(_) => None,
            MirOpKind::CachePut(_) => None,
            MirOpKind::Drop => None,
            MirOpKind::Iterator | MirOpKind::IteratorNext | MirOpKind::Unknown(_) => {
                return Err(format!("unsupported MIR op `{}`", op_kind_label(&op.kind)));
            }
        };

        if let Some(result_id) = op.result {
            let value = result.ok_or_else(|| {
                format!(
                    "MIR op `{}` did not produce value %{}",
                    op_kind_label(&op.kind),
                    result_id.0
                )
            })?;
            self.values.insert(result_id, value);
        }
        Ok(())
    }

    fn eval_call(
        &mut self,
        body: &MirBody,
        callee: &str,
        purity: Purity,
        args: &[MirValueId],
    ) -> Result<InlineValue, String> {
        let runtime_args = args
            .iter()
            .skip(1)
            .map(|arg| self.value(*arg))
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(function) = self
            .module
            .bodies
            .iter()
            .find(|candidate| {
                matches!(candidate.kind, MirBodyKind::Function) && candidate.name == callee
            })
            .cloned()
        {
            let arguments = runtime_args
                .into_iter()
                .map(runtime_value_from_inline)
                .collect::<Vec<_>>();
            let saved_values = self.values.clone();
            let saved_versions = self.versions.clone();
            let result = self.run_body(
                &function,
                &CompiledArtifact {
                    id: self.artifact_id,
                    module: self.module.module.clone(),
                    kind: self.module.kind,
                    optimization: self.module.optimization,
                    optimization_rankings: Vec::new(),
                    parameters: Vec::new(),
                    result_type: None,
                    purity,
                    plan: vox_core::plan::ExecutablePlan::deferred(body.optimization_rank),
                    mir: None,
                    diagnostics: vox_core::diagnostics::DiagnosticBag::default(),
                    dependencies: Vec::new(),
                    source_revision: 0,
                },
                &arguments,
            );
            self.values = saved_values;
            self.versions = saved_versions;
            return result;
        }

        if let Some((package, function)) = split_qualified_host_name(callee) {
            let arguments = runtime_args
                .into_iter()
                .enumerate()
                .map(|(index, value)| HostCallArgument {
                    name: format!("arg{index}"),
                    value: Some(runtime_value_from_inline(value)),
                })
                .collect::<Vec<_>>();
            let result = self
                .runtime
                .invoke_host_function(&package, &function, &arguments)?;
            return Ok(inline_value_from_runtime(&result));
        }

        Err(format!("MIR call target `{callee}` is not executable yet"))
    }

    fn materialize_list(&mut self, values: Vec<InlineValue>) -> InlineValue {
        let summary = HandleSummary {
            type_name: "List".to_owned(),
            summary: render_delimited_inline("[", "]", &values),
            bytes: None,
        };
        let handle = if let Some(data) = handle_data_from_inline_list(&values) {
            self.runtime.allocate_serializable_handle(summary, data)
        } else {
            self.runtime.allocate_handle(summary)
        };
        InlineValue::Handle(handle)
    }

    fn materialize_econ(&mut self, value: InlineValue) -> InlineValue {
        let summary = HandleSummary {
            type_name: "Econ".to_owned(),
            summary: format!("econ({})", render_inline_value(&value)),
            bytes: None,
        };
        InlineValue::Handle(self.runtime.allocate_handle(summary))
    }

    fn arg_values(&self, op: &MirOp) -> Result<Vec<InlineValue>, String> {
        op.args
            .iter()
            .map(|arg| self.value(*arg))
            .collect::<Result<Vec<_>, _>>()
    }

    fn single_arg(&self, op: &MirOp) -> Result<InlineValue, String> {
        let [value] = op.args.as_slice() else {
            return Err(format!(
                "MIR op `{}` expects one argument",
                op_kind_label(&op.kind)
            ));
        };
        self.value(*value)
    }

    fn two_args(&self, op: &MirOp) -> Result<(InlineValue, InlineValue), String> {
        let [left, right] = op.args.as_slice() else {
            return Err(format!(
                "MIR op `{}` expects two arguments",
                op_kind_label(&op.kind)
            ));
        };
        Ok((self.value(*left)?, self.value(*right)?))
    }

    fn value(&self, id: MirValueId) -> Result<InlineValue, String> {
        self.values
            .get(&id)
            .cloned()
            .or_else(|| {
                self.module.bodies.iter().find_map(|body| {
                    body.values
                        .iter()
                        .find(|value| value.id == id)
                        .and_then(|value| match &value.definition {
                            MirValueDefinition::Unit => Some(InlineValue::Tuple(Vec::new())),
                            _ => None,
                        })
                })
            })
            .ok_or_else(|| format!("MIR value %{} is not defined", id.0))
    }

    fn version(&self, id: MirVersionId) -> Result<InlineValue, String> {
        self.versions
            .get(&id)
            .cloned()
            .ok_or_else(|| format!("MIR binding version %v{} is not defined", id.0))
    }
}

fn inline_value_from_runtime(value: &RuntimeValue) -> InlineValue {
    match value {
        RuntimeValue::Inline(value) => value.clone(),
        RuntimeValue::Handle(handle) => InlineValue::Handle(*handle),
    }
}

fn runtime_value_from_inline(value: InlineValue) -> RuntimeValue {
    match value {
        InlineValue::Handle(handle) => RuntimeValue::Handle(handle),
        value => RuntimeValue::Inline(value),
    }
}

fn handle_data_from_inline_list(values: &[InlineValue]) -> Option<HandleData> {
    values
        .iter()
        .map(handle_data_from_inline)
        .collect::<Option<Vec<_>>>()
        .map(HandleData::List)
}

fn handle_data_from_inline(value: &InlineValue) -> Option<HandleData> {
    match value {
        InlineValue::Null => Some(HandleData::Null),
        InlineValue::Bool(value) => Some(HandleData::Bool(*value)),
        InlineValue::Int(value) => Some(HandleData::Int(*value)),
        InlineValue::Float(value) => Some(HandleData::Float(*value)),
        InlineValue::String(value) => Some(HandleData::String(value.clone())),
        InlineValue::Tuple(values) => values
            .iter()
            .map(handle_data_from_inline)
            .collect::<Option<Vec<_>>>()
            .map(HandleData::Tuple),
        InlineValue::Record(fields) => fields
            .iter()
            .map(|(name, value)| Some((name.clone(), handle_data_from_inline(value)?)))
            .collect::<Option<BTreeMap<_, _>>>()
            .map(HandleData::Record),
        InlineValue::Handle(_) => None,
    }
}

fn block_by_id(body: &MirBody, id: MirBlockId) -> Result<&MirBlock, String> {
    body.blocks
        .iter()
        .find(|block| block.id == id)
        .ok_or_else(|| format!("MIR block %bb{} was not found", id.0))
}

fn eval_unary(name: &str, value: InlineValue) -> Result<InlineValue, String> {
    match (name, value) {
        ("negate", InlineValue::Int(value)) => Ok(InlineValue::Int(-value)),
        ("negate", InlineValue::Float(value)) => Ok(InlineValue::Float(-value)),
        ("not", InlineValue::Bool(value)) => Ok(InlineValue::Bool(!value)),
        ("negate", other) => Err(format!(
            "unary `-` is not defined for {}",
            type_name(&other)
        )),
        ("not", other) => Err(format!(
            "unary `!` is not defined for {}",
            type_name(&other)
        )),
        (name, _) => Err(format!("unsupported unary MIR op `{name}`")),
    }
}

fn eval_binary(name: &str, left: InlineValue, right: InlineValue) -> Result<InlineValue, String> {
    match (name, left, right) {
        ("add", InlineValue::Int(left), InlineValue::Int(right)) => {
            Ok(InlineValue::Int(left + right))
        }
        ("subtract", InlineValue::Int(left), InlineValue::Int(right)) => {
            Ok(InlineValue::Int(left - right))
        }
        ("multiply", InlineValue::Int(left), InlineValue::Int(right)) => {
            Ok(InlineValue::Int(left * right))
        }
        ("divide", InlineValue::Int(_), InlineValue::Int(0)) => {
            Err("integer division by zero".to_owned())
        }
        ("remainder", InlineValue::Int(_), InlineValue::Int(0)) => {
            Err("integer remainder by zero".to_owned())
        }
        ("divide", InlineValue::Int(left), InlineValue::Int(right)) => {
            Ok(InlineValue::Int(left / right))
        }
        ("remainder", InlineValue::Int(left), InlineValue::Int(right)) => {
            Ok(InlineValue::Int(left % right))
        }
        ("add", InlineValue::Float(left), InlineValue::Float(right)) => {
            Ok(InlineValue::Float(left + right))
        }
        ("subtract", InlineValue::Float(left), InlineValue::Float(right)) => {
            Ok(InlineValue::Float(left - right))
        }
        ("multiply", InlineValue::Float(left), InlineValue::Float(right)) => {
            Ok(InlineValue::Float(left * right))
        }
        ("divide", InlineValue::Float(left), InlineValue::Float(right)) => {
            Ok(InlineValue::Float(left / right))
        }
        ("remainder", InlineValue::Float(left), InlineValue::Float(right)) => {
            Ok(InlineValue::Float(left % right))
        }
        ("add", InlineValue::Int(left), InlineValue::Float(right)) => {
            Ok(InlineValue::Float(left as f64 + right))
        }
        ("subtract", InlineValue::Int(left), InlineValue::Float(right)) => {
            Ok(InlineValue::Float(left as f64 - right))
        }
        ("multiply", InlineValue::Int(left), InlineValue::Float(right)) => {
            Ok(InlineValue::Float(left as f64 * right))
        }
        ("divide", InlineValue::Int(left), InlineValue::Float(right)) => {
            Ok(InlineValue::Float(left as f64 / right))
        }
        ("remainder", InlineValue::Int(left), InlineValue::Float(right)) => {
            Ok(InlineValue::Float(left as f64 % right))
        }
        ("add", InlineValue::Float(left), InlineValue::Int(right)) => {
            Ok(InlineValue::Float(left + right as f64))
        }
        ("subtract", InlineValue::Float(left), InlineValue::Int(right)) => {
            Ok(InlineValue::Float(left - right as f64))
        }
        ("multiply", InlineValue::Float(left), InlineValue::Int(right)) => {
            Ok(InlineValue::Float(left * right as f64))
        }
        ("divide", InlineValue::Float(left), InlineValue::Int(right)) => {
            Ok(InlineValue::Float(left / right as f64))
        }
        ("remainder", InlineValue::Float(left), InlineValue::Int(right)) => {
            Ok(InlineValue::Float(left % right as f64))
        }
        ("add", InlineValue::String(left), InlineValue::String(right)) => {
            Ok(InlineValue::String(left + &right))
        }
        ("equal", left, right) => Ok(InlineValue::Bool(left == right)),
        ("not_equal", left, right) => Ok(InlineValue::Bool(left != right)),
        ("less", left, right) => compare_values(left, right, |ordering| ordering.is_lt()),
        ("less_equal", left, right) => compare_values(left, right, |ordering| !ordering.is_gt()),
        ("greater", left, right) => compare_values(left, right, |ordering| ordering.is_gt()),
        ("greater_equal", left, right) => compare_values(left, right, |ordering| !ordering.is_lt()),
        (name, left, right) => Err(format!(
            "binary `{name}` is not defined for {} and {}",
            type_name(&left),
            type_name(&right)
        )),
    }
}

fn compare_values(
    left: InlineValue,
    right: InlineValue,
    predicate: impl FnOnce(std::cmp::Ordering) -> bool,
) -> Result<InlineValue, String> {
    let ordering = match (&left, &right) {
        (InlineValue::Int(left), InlineValue::Int(right)) => left.cmp(right),
        (InlineValue::String(left), InlineValue::String(right)) => left.cmp(right),
        _ => {
            return Err(format!(
                "comparison is not defined for {} and {}",
                type_name(&left),
                type_name(&right)
            ));
        }
    };
    Ok(InlineValue::Bool(predicate(ordering)))
}

fn eval_string_interpolation(
    text: &[String],
    values: Vec<InlineValue>,
) -> Result<InlineValue, String> {
    if text.len() != values.len() + 1 {
        return Err(
            "string interpolation text segment count does not match argument count".to_owned(),
        );
    }

    let mut rendered = String::new();
    for (index, value) in values.into_iter().enumerate() {
        rendered.push_str(&text[index]);
        rendered.push_str(&render_inline_value(&value));
    }
    rendered.push_str(
        text.last()
            .expect("string interpolation should have trailing text segment"),
    );
    Ok(InlineValue::String(rendered))
}

fn render_inline_value(value: &InlineValue) -> String {
    match value {
        InlineValue::Null => "null".to_owned(),
        InlineValue::Bool(value) => value.to_string(),
        InlineValue::Int(value) => value.to_string(),
        InlineValue::Float(value) => value.to_string(),
        InlineValue::String(value) => value.clone(),
        InlineValue::Handle(handle) => format!("<handle {}>", handle.0),
        InlineValue::Tuple(values) => match values.as_slice() {
            [] => "()".to_owned(),
            [single] => format!("({},)", render_inline_value(single)),
            _ => render_delimited_inline("(", ")", values),
        },
        InlineValue::Record(fields) => {
            let entries = fields
                .iter()
                .map(|(name, value)| format!("{name} = {}", render_inline_value(value)))
                .collect::<Vec<_>>();
            format!("{{{}}}", entries.join(", "))
        }
    }
}

fn render_delimited_inline(prefix: &str, suffix: &str, values: &[InlineValue]) -> String {
    format!(
        "{prefix}{}{suffix}",
        values
            .iter()
            .map(render_inline_value)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn project_value(value: InlineValue, projection: &MirProjection) -> Result<InlineValue, String> {
    match (value, projection) {
        (InlineValue::Record(fields), MirProjection::Field(field)) => fields
            .get(field)
            .cloned()
            .ok_or_else(|| format!("record does not contain field `{field}`")),
        (InlineValue::Tuple(items), MirProjection::Slot(slot)) => items
            .get(*slot)
            .cloned()
            .ok_or_else(|| format!("tuple index {slot} is out of bounds")),
        (other, MirProjection::Field(field)) => Err(format!(
            "field `{field}` is not supported for {}",
            type_name(&other)
        )),
        (other, MirProjection::Slot(slot)) => Err(format!(
            "slot `{slot}` is not supported for {}",
            type_name(&other)
        )),
    }
}

fn index_value(target: InlineValue, index: InlineValue) -> Result<InlineValue, String> {
    let InlineValue::Int(index) = index else {
        return Err("index expressions require an `Int` index".to_owned());
    };
    let index = usize::try_from(index)
        .map_err(|_| "index expressions require a non-negative index".to_owned())?;
    match target {
        InlineValue::Tuple(items) => items
            .get(index)
            .cloned()
            .ok_or_else(|| format!("tuple index {index} is out of bounds")),
        other => Err(format!(
            "indexing is not supported for {}",
            type_name(&other)
        )),
    }
}

fn apply_updated_path(
    target: InlineValue,
    path: &[MirPathSegment],
    replacement: InlineValue,
) -> Result<InlineValue, String> {
    let Some((segment, rest)) = path.split_first() else {
        return Err("updated path cannot be empty".to_owned());
    };
    match (target, segment) {
        (InlineValue::Record(mut fields), MirPathSegment::Field(name)) => {
            let current = fields
                .get(name)
                .cloned()
                .ok_or_else(|| format!("record does not contain field `{name}`"))?;
            let next = if rest.is_empty() {
                replacement
            } else {
                apply_updated_path(current, rest, replacement)?
            };
            fields.insert(name.clone(), next);
            Ok(InlineValue::Record(fields))
        }
        (InlineValue::Tuple(mut items), MirPathSegment::Index(index)) => {
            let slot = items
                .get_mut(*index)
                .ok_or_else(|| format!("tuple index {index} is out of bounds"))?;
            *slot = if rest.is_empty() {
                replacement
            } else {
                apply_updated_path(slot.clone(), rest, replacement)?
            };
            Ok(InlineValue::Tuple(items))
        }
        (other, _) => Err(format!(
            "updated is not supported for {}",
            type_name(&other)
        )),
    }
}

fn expect_bool(value: InlineValue, label: &str) -> Result<bool, String> {
    if let InlineValue::Bool(value) = value {
        Ok(value)
    } else {
        Err(format!("{label} must evaluate to `Bool`"))
    }
}

fn matches_type_name(value: &InlineValue, ty: &str) -> bool {
    match (ty, value) {
        ("Int", InlineValue::Int(_))
        | ("Float", InlineValue::Float(_))
        | ("Bool", InlineValue::Bool(_))
        | ("String", InlineValue::String(_))
        | ("Null", InlineValue::Null) => true,
        ("Unit", InlineValue::Tuple(items)) => items.is_empty(),
        _ => false,
    }
}

fn split_qualified_host_name(callee: &str) -> Option<(ModulePath, String)> {
    let (package, function) = callee.rsplit_once('.')?;
    Some((ModulePath::parse(package).ok()?, function.to_owned()))
}

fn type_name(value: &InlineValue) -> &'static str {
    match value {
        InlineValue::Int(_) => "Int",
        InlineValue::Float(_) => "Float",
        InlineValue::Bool(_) => "Bool",
        InlineValue::String(_) => "String",
        InlineValue::Handle(_) => "Handle",
        InlineValue::Tuple(_) => "Tuple",
        InlineValue::Record(_) => "Record",
        InlineValue::Null => "Null",
    }
}

fn op_kind_label(kind: &MirOpKind) -> &'static str {
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
