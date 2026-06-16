use crate::types::VoxType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinReceiver {
    Int,
    UInt,
    Float,
    Bool,
    String,
    List,
    Econ,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinImpl {
    Intrinsic,
    Prelude,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinMethod {
    pub receiver: BuiltinReceiver,
    pub name: &'static str,
    pub implementation: BuiltinImpl,
}

pub const BUILTIN_METHODS: &[BuiltinMethod] = &[
    BuiltinMethod {
        receiver: BuiltinReceiver::Int,
        name: "toString",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::Int,
        name: "toFloat",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::Int,
        name: "toUInt",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::UInt,
        name: "toString",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::UInt,
        name: "toFloat",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::UInt,
        name: "toInt",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::Float,
        name: "toString",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::Float,
        name: "toInt",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::Float,
        name: "round",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::Float,
        name: "floor",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::Float,
        name: "ceil",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::Bool,
        name: "toString",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::String,
        name: "length",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::String,
        name: "isEmpty",
        implementation: BuiltinImpl::Prelude,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::String,
        name: "toInt",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::String,
        name: "toFloat",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::String,
        name: "startsWith",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::String,
        name: "endsWith",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::String,
        name: "contains",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::String,
        name: "indexOf",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::String,
        name: "substring",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::String,
        name: "replace",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::String,
        name: "split",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::String,
        name: "toLower",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::String,
        name: "toUpper",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::String,
        name: "trim",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::String,
        name: "repeat",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::List,
        name: "length",
        implementation: BuiltinImpl::Intrinsic,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::List,
        name: "size",
        implementation: BuiltinImpl::Prelude,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::List,
        name: "isEmpty",
        implementation: BuiltinImpl::Prelude,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::List,
        name: "first",
        implementation: BuiltinImpl::Prelude,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::List,
        name: "last",
        implementation: BuiltinImpl::Prelude,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::List,
        name: "get",
        implementation: BuiltinImpl::Prelude,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::List,
        name: "contains",
        implementation: BuiltinImpl::Prelude,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::List,
        name: "indexOf",
        implementation: BuiltinImpl::Prelude,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::List,
        name: "slice",
        implementation: BuiltinImpl::Prelude,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::List,
        name: "reversed",
        implementation: BuiltinImpl::Prelude,
    },
    BuiltinMethod {
        receiver: BuiltinReceiver::Econ,
        name: "update",
        implementation: BuiltinImpl::Intrinsic,
    },
];

pub fn builtin_method(receiver: BuiltinReceiver, name: &str) -> Option<&'static BuiltinMethod> {
    BUILTIN_METHODS
        .iter()
        .find(|method| method.receiver == receiver && method.name == name)
}

pub fn builtin_methods_for_receiver(
    receiver: BuiltinReceiver,
) -> impl Iterator<Item = &'static BuiltinMethod> {
    BUILTIN_METHODS
        .iter()
        .filter(move |method| method.receiver == receiver)
}

pub fn builtin_callee_name(receiver: BuiltinReceiver, method_name: &str) -> String {
    format!("__builtin__{}::{method_name}", receiver.name())
}

pub fn split_builtin_callee(callee: &str) -> Option<(BuiltinReceiver, &str)> {
    let rest = callee.strip_prefix("__builtin__")?;
    let (receiver, method) = rest.split_once("::")?;
    Some((BuiltinReceiver::from_name(receiver)?, method))
}

impl BuiltinReceiver {
    pub fn name(self) -> &'static str {
        match self {
            Self::Int => "Int",
            Self::UInt => "UInt",
            Self::Float => "Float",
            Self::Bool => "Bool",
            Self::String => "String",
            Self::List => "List",
            Self::Econ => "Econ",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "Int" => Some(Self::Int),
            "UInt" => Some(Self::UInt),
            "Float" => Some(Self::Float),
            "Bool" => Some(Self::Bool),
            "String" => Some(Self::String),
            "List" => Some(Self::List),
            "Econ" => Some(Self::Econ),
            _ => None,
        }
    }
}

pub fn builtin_receiver_for_type(ty: &VoxType) -> Option<BuiltinReceiver> {
    match ty {
        VoxType::Int => Some(BuiltinReceiver::Int),
        VoxType::UInt => Some(BuiltinReceiver::UInt),
        VoxType::Float => Some(BuiltinReceiver::Float),
        VoxType::Bool => Some(BuiltinReceiver::Bool),
        VoxType::String => Some(BuiltinReceiver::String),
        VoxType::List(_) => Some(BuiltinReceiver::List),
        VoxType::Named(name) if name.name == "Econ" => Some(BuiltinReceiver::Econ),
        VoxType::OpaqueSurface(name) if name == "Econ" => Some(BuiltinReceiver::Econ),
        _ => None,
    }
}
