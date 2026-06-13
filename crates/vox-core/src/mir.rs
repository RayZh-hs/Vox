use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::{self, Write},
};

use crate::{
    diagnostics::TextSpan,
    host::Purity,
    opt::{OptimizationLevel, OptimizationRank},
    source::{ModuleKind, ModulePath},
    types::VoxType,
    value::InlineValue,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MirBodyId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MirBlockId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MirBindingId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MirVersionId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MirValueId(pub u32);

#[derive(Debug, Clone, PartialEq)]
pub struct MirModule {
    pub module: ModulePath,
    pub kind: ModuleKind,
    pub optimization: OptimizationLevel,
    pub bodies: Vec<MirBody>,
}

impl MirModule {
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "module {} kind={} opt={:?}",
            self.module.as_str(),
            match self.kind {
                ModuleKind::Package => "package",
                ModuleKind::Script { evil: false } => "script",
                ModuleKind::Script { evil: true } => "evil-script",
            },
            self.optimization
        );
        for body in &self.bodies {
            let _ = writeln!(out);
            body.write_text(&mut out)
                .expect("writing MIR text to a string should not fail");
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MirBody {
    pub id: MirBodyId,
    pub name: String,
    pub kind: MirBodyKind,
    pub span: Option<TextSpan>,
    pub purity: Purity,
    pub optimization_rank: OptimizationRank,
    pub parameters: Vec<MirValueId>,
    pub captures: Vec<MirCapture>,
    pub bindings: Vec<MirBinding>,
    pub versions: Vec<MirBindingVersion>,
    pub values: Vec<MirValue>,
    pub blocks: Vec<MirBlock>,
    pub result_type: Option<VoxType>,
    pub analyses: MirAnalysisSummary,
}

impl MirBody {
    pub fn write_text(&self, out: &mut String) -> fmt::Result {
        writeln!(
            out,
            "body @{} kind={} purity={} rank={} {{",
            self.name,
            self.kind.as_str(),
            match self.purity {
                Purity::Pure => "pure",
                Purity::Evil => "evil",
            },
            self.optimization_rank.as_str()
        )?;
        self.analyses.write_text(out)?;
        for binding in &self.bindings {
            write!(
                out,
                "  binding %b{} {} {} scope={} versions=[",
                binding.id.0,
                match binding.mutability {
                    MirMutability::Val => "val",
                    MirMutability::Var => "var",
                },
                binding.name,
                binding.scope_depth
            )?;
            for (index, version) in binding.versions.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write!(out, "%v{}", version.0)?;
            }
            writeln!(out, "]")?;
        }
        if !self.bindings.is_empty() {
            writeln!(out)?;
        }
        for block in &self.blocks {
            writeln!(out, "  block %bb{}:", block.id.0)?;
            if !block.parameters.is_empty() {
                write!(out, "    params ")?;
                write_value_list(out, &block.parameters)?;
                writeln!(out)?;
            }
            for op in &block.ops {
                write!(out, "    ")?;
                op.write_text(out)?;
                if let Some(result) = op.result {
                    if let Some(value) = self.values.iter().find(|value| value.id == result) {
                        value.write_analysis_text(out)?;
                    }
                }
                writeln!(out)?;
            }
            write!(out, "    ")?;
            block.terminator.write_text(out)?;
            writeln!(out)?;
        }
        writeln!(out, "}}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirBodyKind {
    ScriptEntry,
    ValueInitializer,
    Function,
    Lambda,
}

impl MirBodyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ScriptEntry => "script_entry",
            Self::ValueInitializer => "value_initializer",
            Self::Function => "function",
            Self::Lambda => "lambda",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MirCapture {
    pub name: String,
    pub value: MirValueId,
    pub ty: Option<VoxType>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MirBinding {
    pub id: MirBindingId,
    pub name: String,
    pub mutability: MirMutability,
    pub scope_depth: u32,
    pub declared_type: Option<VoxType>,
    pub span: Option<TextSpan>,
    pub capture: MirCaptureMode,
    pub versions: Vec<MirVersionId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirMutability {
    Val,
    Var,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirCaptureMode {
    Local,
    Captured,
    NonCapturable,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MirBindingVersion {
    pub id: MirVersionId,
    pub binding: MirBindingId,
    pub value: MirValueId,
    pub source: MirVersionSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirVersionSource {
    Initializer,
    Assignment,
    CompoundAssignment,
    Join,
    Loop,
    Parameter,
    Capture,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MirValue {
    pub id: MirValueId,
    pub ty: Option<VoxType>,
    pub definition: MirValueDefinition,
    pub span: Option<TextSpan>,
    pub binding_version: Option<MirVersionId>,
    pub uses: Vec<MirUse>,
    pub lifetime: MirLifetime,
    pub escape: MirEscape,
    pub demand: MirDemand,
    pub storage: MirStorage,
}

impl MirValue {
    fn write_analysis_text(&self, out: &mut String) -> fmt::Result {
        let mut parts = Vec::new();
        if let Some(version) = self.binding_version {
            parts.push(format!("version=%v{}", version.0));
        }
        if self.lifetime.first.is_some()
            || self.lifetime.last.is_some()
            || self.lifetime.reusable_after_last_use
            || !self.lifetime.live_in.is_empty()
            || !self.lifetime.live_out.is_empty()
        {
            parts.push(format!(
                "lifetime={}..{}{}",
                render_program_point(self.lifetime.first),
                render_program_point(self.lifetime.last),
                if self.lifetime.reusable_after_last_use {
                    " reusable"
                } else {
                    ""
                }
            ));
        }
        if self.escape.escapes() {
            let mut escapes = Vec::new();
            if self.escape.returned {
                escapes.push("return");
            }
            if self.escape.captured {
                escapes.push("capture");
            }
            if self.escape.econ {
                escapes.push("econ");
            }
            if self.escape.evil_call {
                escapes.push("evil_call");
            }
            if self.escape.host_boundary {
                escapes.push("host");
            }
            parts.push(format!("escapes={}", escapes.join("|")));
        }
        if !matches!(self.demand, MirDemand::Unknown) {
            parts.push(format!("demand={}", render_demand(&self.demand)));
        }
        if !matches!(self.storage, MirStorage::Fresh) {
            parts.push(format!("storage={}", render_storage(&self.storage)));
        }
        if !parts.is_empty() {
            write!(out, " ; {}", parts.join(", "))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum MirValueDefinition {
    Parameter(String),
    Capture(String),
    BlockParameter(MirBlockId),
    Op,
    Literal,
    Unit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirUse {
    pub block: MirBlockId,
    pub op_index: Option<u32>,
    pub kind: MirUseKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirUseKind {
    Operand,
    Condition,
    Return,
    Escape,
    Drop,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MirLifetime {
    pub first: Option<MirProgramPoint>,
    pub last: Option<MirProgramPoint>,
    pub live_in: BTreeSet<MirBlockId>,
    pub live_out: BTreeSet<MirBlockId>,
    pub reusable_after_last_use: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MirProgramPoint {
    pub block: MirBlockId,
    pub index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MirEscape {
    pub returned: bool,
    pub captured: bool,
    pub econ: bool,
    pub evil_call: bool,
    pub host_boundary: bool,
}

impl MirEscape {
    pub fn escapes(&self) -> bool {
        self.returned || self.captured || self.econ || self.evil_call || self.host_boundary
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirDemand {
    Unknown,
    Full,
    Projection {
        fields: BTreeSet<String>,
        slots: BTreeSet<usize>,
    },
    None,
}

impl Default for MirDemand {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirStorage {
    Fresh,
    Reuse(MirValueId),
    CopyOnWrite(MirValueId),
    Virtual,
}

impl Default for MirStorage {
    fn default() -> Self {
        Self::Fresh
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MirBlock {
    pub id: MirBlockId,
    pub name: String,
    pub parameters: Vec<MirValueId>,
    pub ops: Vec<MirOp>,
    pub terminator: MirTerminator,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MirOp {
    pub result: Option<MirValueId>,
    pub kind: MirOpKind,
    pub args: Vec<MirValueId>,
    pub span: Option<TextSpan>,
}

impl MirOp {
    fn write_text(&self, out: &mut String) -> fmt::Result {
        if let Some(result) = self.result {
            write!(out, "%{} = ", result.0)?;
        }
        self.kind.write_text(out)?;
        if !self.args.is_empty() {
            out.push(' ');
            write_value_list(out, &self.args)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum MirOpKind {
    Literal(InlineValue),
    Unit,
    Use(MirVersionId),
    Bind(MirVersionId),
    Unary(String),
    Binary(String),
    Tuple { shape: usize },
    Record { fields: Vec<String> },
    List,
    StringInterpolate { text: Vec<String> },
    Project(MirProjection),
    Index,
    Updated { path: Vec<MirPathSegment> },
    Call { callee: String, purity: Purity },
    Lambda {
        parameters: Vec<String>,
        captures: Vec<MirValueId>,
        body_id: MirBodyId,
    },
    Econ { ty: String },
    NonNull,
    SafeProject(String),
    TypeTest(String),
    TypeRefine(String),
    Iterator,
    IteratorNext,
    CacheGet(String),
    CachePut(String),
    Drop,
    Unknown(String),
}

impl MirOpKind {
    fn write_text(&self, out: &mut String) -> fmt::Result {
        match self {
            Self::Literal(value) => write!(out, "literal {}", render_inline_value(value)),
            Self::Unit => {
                out.push_str("unit");
                Ok(())
            }
            Self::Use(version) => write!(out, "use %v{}", version.0),
            Self::Bind(version) => write!(out, "bind %v{}", version.0),
            Self::Unary(op) => write!(out, "unary {op}"),
            Self::Binary(op) => write!(out, "binary {op}"),
            Self::Tuple { shape } => write!(out, "tuple shape={shape}"),
            Self::Record { fields } => write!(out, "record {{{}}}", fields.join(",")),
            Self::List => {
                out.push_str("list");
                Ok(())
            }
            Self::StringInterpolate { text } => {
                write!(out, "string_interpolate text={:?}", text)
            }
            Self::Project(projection) => match projection {
                MirProjection::Field(field) => write!(out, "project .{field}"),
                MirProjection::Slot(slot) => write!(out, "project #{slot}"),
            },
            Self::Index => {
                out.push_str("index");
                Ok(())
            }
            Self::Updated { path } => {
                out.push_str("updated ");
                write_path(out, path)
            }
            Self::Call { callee, purity } => write!(
                out,
                "call {} purity={}",
                callee,
                match purity {
                    Purity::Pure => "pure",
                    Purity::Evil => "evil",
                }
            ),
            Self::Lambda {
                parameters,
                captures,
                body_id,
            } => write!(
                out,
                "lambda ({}) captures=[{}] body=@{}",
                parameters.join(","),
                captures
                    .iter()
                    .map(|v| format!("%{}", v.0))
                    .collect::<Vec<_>>()
                    .join(","),
                body_id.0
            ),
            Self::Econ { ty } => write!(out, "econ {ty}"),
            Self::NonNull => {
                out.push_str("non_null");
                Ok(())
            }
            Self::SafeProject(field) => write!(out, "safe_project .{field}"),
            Self::TypeTest(ty) => write!(out, "type_test {ty}"),
            Self::TypeRefine(ty) => write!(out, "type_refine {ty}"),
            Self::Iterator => {
                out.push_str("iterator");
                Ok(())
            }
            Self::IteratorNext => {
                out.push_str("iterator_next");
                Ok(())
            }
            Self::CacheGet(key) => write!(out, "cache_get {key}"),
            Self::CachePut(key) => write!(out, "cache_put {key}"),
            Self::Drop => {
                out.push_str("drop");
                Ok(())
            }
            Self::Unknown(name) => write!(out, "unknown {name}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirProjection {
    Field(String),
    Slot(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirPathSegment {
    Field(String),
    Index(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub enum MirTerminator {
    Jump {
        target: MirBlockId,
        args: Vec<MirValueId>,
    },
    Branch {
        condition: MirValueId,
        then_target: MirBlockId,
        then_args: Vec<MirValueId>,
        else_target: MirBlockId,
        else_args: Vec<MirValueId>,
    },
    Return(MirValueId),
    Panic(String),
    Unreachable,
}

impl MirTerminator {
    fn write_text(&self, out: &mut String) -> fmt::Result {
        match self {
            Self::Jump { target, args } => {
                write!(out, "jump %bb{}", target.0)?;
                if !args.is_empty() {
                    out.push(' ');
                    write_value_list(out, args)?;
                }
                Ok(())
            }
            Self::Branch {
                condition,
                then_target,
                then_args,
                else_target,
                else_args,
            } => {
                write!(out, "branch %{} then %bb{}", condition.0, then_target.0)?;
                if !then_args.is_empty() {
                    out.push(' ');
                    write_value_list(out, then_args)?;
                }
                write!(out, " else %bb{}", else_target.0)?;
                if !else_args.is_empty() {
                    out.push(' ');
                    write_value_list(out, else_args)?;
                }
                Ok(())
            }
            Self::Return(value) => write!(out, "return %{}", value.0),
            Self::Panic(message) => write!(out, "panic {:?}", message),
            Self::Unreachable => {
                out.push_str("unreachable");
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MirAnalysisSummary {
    pub def_use_complete: bool,
    pub lifetimes_complete: bool,
    pub demand_complete: bool,
    pub active_value_cache_enabled: bool,
    pub sealed: bool,
    pub culled_values: u32,
    pub reused_slots: u32,
    pub copy_on_write_values: u32,
    pub value_slots: BTreeMap<MirValueId, u32>,
}

impl MirAnalysisSummary {
    fn write_text(&self, out: &mut String) -> fmt::Result {
        let mut flags = Vec::new();
        if self.def_use_complete {
            flags.push("def_use");
        }
        if self.lifetimes_complete {
            flags.push("lifetimes");
        }
        if self.demand_complete {
            flags.push("demand");
        }
        if self.active_value_cache_enabled {
            flags.push("active_cache");
        }
        if self.sealed {
            flags.push("sealed");
        }
        if flags.is_empty()
            && self.culled_values == 0
            && self.reused_slots == 0
            && self.copy_on_write_values == 0
            && self.value_slots.is_empty()
        {
            return Ok(());
        }

        writeln!(out, "  analyses flags=[{}]", flags.join(","))?;
        if self.culled_values != 0
            || self.reused_slots != 0
            || self.copy_on_write_values != 0
            || !self.value_slots.is_empty()
        {
            write!(
                out,
                "  analysis_counts culled={} reused_slots={} copy_on_write={}",
                self.culled_values, self.reused_slots, self.copy_on_write_values
            )?;
            if !self.value_slots.is_empty() {
                out.push_str(" slots=[");
                for (index, (value, slot)) in self.value_slots.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write!(out, "%{}:s{}", value.0, slot)?;
                }
                out.push(']');
            }
            writeln!(out)?;
        }
        Ok(())
    }
}

fn write_value_list(out: &mut String, values: &[MirValueId]) -> fmt::Result {
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        write!(out, "%{}", value.0)?;
    }
    Ok(())
}

fn write_path(out: &mut String, path: &[MirPathSegment]) -> fmt::Result {
    for (index, segment) in path.iter().enumerate() {
        if index > 0 {
            out.push('.');
        }
        match segment {
            MirPathSegment::Field(field) => out.push_str(field),
            MirPathSegment::Index(slot) => write!(out, "#{slot}")?,
        }
    }
    Ok(())
}

fn render_inline_value(value: &InlineValue) -> String {
    match value {
        InlineValue::Int(value) => value.to_string(),
        InlineValue::Float(value) => value.to_string(),
        InlineValue::Bool(value) => value.to_string(),
        InlineValue::String(value) => format!("{value:?}"),
        InlineValue::Tuple(values) => {
            let values = values
                .iter()
                .map(render_inline_value)
                .collect::<Vec<_>>()
                .join(", ");
            format!("({values})")
        }
        InlineValue::Record(fields) => {
            let fields = fields
                .iter()
                .map(|(name, value)| format!("{name}: {}", render_inline_value(value)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{fields}}}")
        }
        InlineValue::Handle(handle) => format!("<handle {}>", handle.0),
        InlineValue::Null => "null".to_owned(),
    }
}

fn render_program_point(point: Option<MirProgramPoint>) -> String {
    point
        .map(|point| format!("%bb{}:{}", point.block.0, point.index))
        .unwrap_or_else(|| "?".to_owned())
}

fn render_demand(demand: &MirDemand) -> String {
    match demand {
        MirDemand::Unknown => "unknown".to_owned(),
        MirDemand::Full => "full".to_owned(),
        MirDemand::None => "none".to_owned(),
        MirDemand::Projection { fields, slots } => {
            let mut parts = Vec::new();
            if !fields.is_empty() {
                parts.push(format!(
                    "fields({})",
                    fields.iter().cloned().collect::<Vec<_>>().join("|")
                ));
            }
            if !slots.is_empty() {
                parts.push(format!(
                    "slots({})",
                    slots
                        .iter()
                        .map(|slot| slot.to_string())
                        .collect::<Vec<_>>()
                        .join("|")
                ));
            }
            if parts.is_empty() {
                "projection(empty)".to_owned()
            } else {
                format!("projection:{}", parts.join("+"))
            }
        }
    }
}

fn render_storage(storage: &MirStorage) -> String {
    match storage {
        MirStorage::Fresh => "fresh".to_owned(),
        MirStorage::Reuse(value) => format!("reuse(%{})", value.0),
        MirStorage::CopyOnWrite(value) => format!("cow(%{})", value.0),
        MirStorage::Virtual => "virtual".to_owned(),
    }
}
