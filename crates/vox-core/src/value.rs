use std::collections::BTreeMap;

use crate::ids::HandleId;

#[derive(Debug, Clone, PartialEq)]
pub enum InlineValue {
    Int(i64),
    UInt(u64),
    Float(f64),
    Bool(bool),
    String(String),
    Tuple(Vec<InlineValue>),
    Record(BTreeMap<String, InlineValue>),
    Handle(HandleId),
    Null,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HandleData {
    Int(i64),
    UInt(u64),
    Float(f64),
    Bool(bool),
    String(String),
    List(Vec<HandleData>),
    Tuple(Vec<HandleData>),
    Record(BTreeMap<String, HandleData>),
    Null,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandleSummary {
    pub type_name: String,
    pub summary: String,
    pub bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeValue {
    Inline(InlineValue),
    Handle(HandleId),
}
